//! Debounced filesystem watcher for an Obsidian vault.
//!
//! Watches a vault root recursively, filters to `.md` file events,
//! ignores configured directories, and debounces rapid changes into
//! batched notifications. Used by both oracle (live reindex) and
//! cortex (daemon sweep triggers).

use eyre::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Far-future offset used to park the debounce timer when no events are
/// pending. `Duration::MAX` cannot be used: tokio computes `Instant::now() +
/// duration` eagerly and `now + Duration::MAX` overflows and panics, killing
/// the debounce task after its first emitted batch (empirically confirmed).
const INERT_DEBOUNCE: Duration = Duration::from_secs(86400 * 365);

/// Configuration for the vault file watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Seconds to wait after last event before firing (default: 5).
    pub debounce_secs: u64,
    /// Directory names to ignore (e.g., [".git", ".obsidian", "templates"]).
    pub ignore_dirs: Vec<String>,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        // `quarantine` is where audit `--fix duplicate` parks set-aside notes;
        // ignoring it here keeps every move event from waking the debounce
        // loop for paths the indexer would only filter out anyway.
        Self {
            debounce_secs: 5,
            ignore_dirs: [".git", ".obsidian", "templates", "quarantine"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// A batch of changed paths emitted after the debounce window closes.
#[derive(Debug, Clone)]
pub struct VaultChange {
    pub changed_paths: Vec<PathBuf>,
}

/// A debounced filesystem watcher for an Obsidian vault.
///
/// Watches recursively, filters to `.md` files, debounces rapid changes,
/// and sends batched paths through a channel. Must be held alive for the
/// duration of watching - dropping it stops the notify watcher and the
/// debounce task exits when its input channel closes.
pub struct VaultWatcher {
    // Held for ownership - dropping stops the filesystem watcher. The `_`
    // prefix is the drop-guard carve-out: never read by name, kept alive so
    // its Drop tears down the notify watcher when VaultWatcher is dropped.
    _watcher: RecommendedWatcher,
}

impl VaultWatcher {
    /// Start watching a vault root directory.
    ///
    /// Returns the watcher handle and a receiver for debounced change events.
    /// Internally spawns a tokio task that collects raw notify events,
    /// filters them, and runs the debounce timer.
    ///
    /// The optional `applying` flag lets callers suppress events during
    /// their own vault writes (prevents feedback loops). Pass `None` if
    /// the consumer is read-only (e.g., oracle).
    pub fn start(
        vault_root: &Path,
        config: WatcherConfig,
        applying: Option<Arc<AtomicBool>>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<VaultChange>)> {
        // Internal channel: raw notify events -> debounce task
        let (raw_tx, raw_rx) = mpsc::unbounded_channel::<notify::Event>();

        // Output channel: debounced batches -> consumer
        let (out_tx, out_rx) = mpsc::unbounded_channel::<VaultChange>();

        // Clone applying for the notify callback thread
        let applying_for_callback = applying.clone();

        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                // If applying flag is set, discard events silently
                if let Some(ref flag) = applying_for_callback
                    && flag.load(Ordering::Relaxed)
                {
                    return;
                }
                if let Ok(event) = res {
                    let _ = raw_tx.send(event);
                }
            })
            .context("failed to create filesystem watcher")?;

        watcher
            .watch(vault_root.as_ref(), RecursiveMode::Recursive)
            .context("failed to watch vault root")?;

        log::info!("vault watcher started: {}", vault_root.display());

        // Spawn the debounce task
        let ignore_dirs = config.ignore_dirs.clone();
        let debounce_duration = Duration::from_secs(config.debounce_secs);
        tokio::spawn(debounce_loop(raw_rx, out_tx, ignore_dirs, debounce_duration));

        Ok((Self { _watcher: watcher }, out_rx))
    }
}

/// The debounce loop: receives raw notify events, filters them, accumulates
/// changed paths, and emits batched VaultChange after a quiet period.
async fn debounce_loop(
    mut raw_rx: mpsc::UnboundedReceiver<notify::Event>,
    out_tx: mpsc::UnboundedSender<VaultChange>,
    ignore_dirs: Vec<String>,
    debounce_duration: Duration,
) {
    let mut pending: Vec<PathBuf> = Vec::new();
    // O(1) dedup membership alongside `pending` (preserves insertion order). A
    // linear `pending.contains` went quadratic during Syncthing bulk syncs that
    // dump thousands of events into one debounce window.
    let mut seen: HashSet<PathBuf> = HashSet::new();

    // Debounce timer: starts inert (far future), reset when events arrive
    let debounce = tokio::time::sleep(INERT_DEBOUNCE);
    tokio::pin!(debounce);

    loop {
        tokio::select! {
            event = raw_rx.recv() => {
                let Some(event) = event else {
                    // Channel closed (watcher dropped) - exit
                    break;
                };

                if !should_process_event(&event, &ignore_dirs) {
                    continue;
                }

                // Collect .md file paths from this event (O(1) dedup via `seen`)
                for path in &event.paths {
                    if path.extension().and_then(|e| e.to_str()) == Some("md")
                        && seen.insert(path.clone())
                    {
                        pending.push(path.clone());
                    }
                }

                if !pending.is_empty() {
                    // Reset debounce timer
                    debounce.as_mut().reset(Instant::now() + debounce_duration);
                }
            }
            () = &mut debounce, if !pending.is_empty() => {
                // Debounce fired - emit batch
                log::info!("vault watcher debounce fired: {} file(s)", pending.len());
                let change = VaultChange {
                    changed_paths: std::mem::take(&mut pending),
                };
                seen.clear();
                if out_tx.send(change).is_err() {
                    // Consumer dropped - exit
                    break;
                }
                // Make debounce inert again
                debounce.as_mut().reset(Instant::now() + INERT_DEBOUNCE);
            }
        }
    }

    log::info!("vault watcher debounce loop exiting");
}

/// Check if a filesystem event should be processed.
fn should_process_event(event: &notify::Event, ignore_dirs: &[String]) -> bool {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return false,
    }

    // Check if any path is in an ignored directory
    for path in &event.paths {
        for component in path.components() {
            let name = component.as_os_str().to_string_lossy();
            if ignore_dirs.iter().any(|ig| name == *ig) {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests;
