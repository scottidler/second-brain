//! Watermark + durable identity (design doc Architecture > Watermark + durable
//! identity). A JSON state file holds the export `cursor` plus, per PUBLISHED
//! session id, `{ note_path, n_msgs, body_hash }`. The body hash is over the
//! INPUT body fed to the distiller - never the distillation OUTPUT (an LLM pass
//! is nondeterministic, so an output hash can never anchor identity, round-2
//! panel finding).
//!
//! Re-appearance semantics for a published id past the cursor:
//! - cheap filter: `n-msgs` unchanged -> Skip WITHOUT fetching the body
//! - otherwise fetch + hash: hash changed -> follow-up note; hash unchanged ->
//!   Skip, but ADVANCE the snapshot's `n-msgs` so it never re-checks run after
//!   run ("every processed appearance advances the published snapshot")
//! - `--force`: re-distill regardless (a fresh distillation is the whole point)
//!
//! Locking: the state takes an EXCLUSIVE advisory lock (a dedicated sibling
//! `.lock` file, mirroring cortex's `embed.lock`), so a nightly timer run and a
//! hand-run fail loudly instead of racing the cursor.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use eyre::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::contract::BodyMessage;

/// One published session's durable snapshot. `n_msgs` and `body_hash` are the
/// identity anchors; `note_path` is the landed note (for a follow-up's
/// back-link and for advancing a snapshot in place on an unchanged re-appear).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishedEntry {
    pub note_path: String,
    pub n_msgs: i64,
    pub body_hash: String,
    /// The trace that produced this snapshot (design doc
    /// `2026-08-15-harvest-note-identity-trace-keyed-replace.md`, Phase 2/4):
    /// lets a follow-up resolve its prior note through
    /// `identity::resolve_prior_note` instead of trusting a stale
    /// `note_path`. `serde(default)` so on-disk state written before this
    /// field existed keeps deserializing (those rows read back as `None`
    /// until their next publish).
    #[serde(default)]
    pub trace: Option<String>,
}

/// The on-disk harvest state. `published` is a `BTreeMap` (not `HashMap`) so
/// the serialized JSON is stable/diffable and iteration order is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct WatermarkState {
    /// Last export cursor consumed. `None` = fresh install (first-run backfill
    /// uses `harvest.initial-since` instead of a cursor).
    #[serde(default)]
    pub cursor: Option<i64>,
    /// Published session id -> snapshot.
    #[serde(default)]
    pub published: BTreeMap<String, PublishedEntry>,
}

impl WatermarkState {
    /// Load state from `path`, returning the default (empty) state when the
    /// file is absent - a fresh install is not an error. A present-but-corrupt
    /// file IS a loud error (never silently reset the cursor and re-inhale
    /// history).
    pub fn load(path: &Path) -> Result<Self> {
        log::debug!("harvest::WatermarkState::load: path={}", path.display());
        match std::fs::read(path) {
            Ok(bytes) => {
                let state: WatermarkState = serde_json::from_slice(&bytes)
                    .with_context(|| format!("harvest state file {} is corrupt", path.display()))?;
                log::debug!(
                    "harvest::WatermarkState::load: cursor={:?} published={}",
                    state.cursor,
                    state.published.len()
                );
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("harvest::WatermarkState::load: no state file yet (fresh install)");
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| format!("failed to read harvest state {}", path.display())),
        }
    }

    /// Atomically AND DURABLY persist state via `vault::note::write_atomic`
    /// (temp file in the target's own directory, fsynced, renamed into place,
    /// parent directory fsynced). The prior `fs::write` + `fs::rename` pair
    /// fsynced neither the temp file nor the parent dir, so the note could
    /// survive a power loss while the record that it exists did not
    /// (durability inverted) - Phase 2 of the trace-keyed-replace design.
    pub fn save(&self, path: &Path) -> Result<()> {
        log::debug!(
            "harvest::WatermarkState::save: path={} cursor={:?} published={}",
            path.display(),
            self.cursor,
            self.published.len()
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create harvest state dir {}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(self).context("serialize harvest state")?;
        vault::note::write_atomic(path, &json)
            .with_context(|| format!("durably write harvest state {}", path.display()))?;
        Ok(())
    }
}

/// The state file is held by another harvest process (nightly timer vs
/// hand-run). A typed marker error (not a message substring) so callers detect
/// contention by type. Mirrors cortex's `EmbedLockHeld`.
#[derive(Debug)]
pub struct HarvestLockHeld {
    pub path: PathBuf,
}

impl std::fmt::Display for HarvestLockHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "harvest state lock held by another process: {} - a nightly timer run and a hand-run cannot race the cursor",
            self.path.display()
        )
    }
}

impl std::error::Error for HarvestLockHeld {}

/// RAII exclusive lock over the harvest state. The advisory lock lives on a
/// dedicated sibling `.lock` file (never rewritten), so it survives the
/// atomic temp+rename of the state JSON. Released on drop (process exit
/// included).
#[derive(Debug)]
pub struct HarvestLock {
    _file: File,
}

/// Acquire the exclusive lock for the state file at `state_path`. Fails loudly
/// with [`HarvestLockHeld`] if another process holds it.
pub fn acquire_lock(state_path: &Path) -> Result<HarvestLock> {
    let lock_path = lock_path_for(state_path);
    log::debug!("harvest::acquire_lock: lock_path={}", lock_path.display());
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create harvest lock dir {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open harvest lock file {}", lock_path.display()))?;
    file.try_lock_exclusive().map_err(|_| HarvestLockHeld {
        path: lock_path.clone(),
    })?;
    Ok(HarvestLock { _file: file })
}

fn lock_path_for(state_path: &Path) -> PathBuf {
    let mut name = state_path
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("harvest-state.json"));
    name.push(".lock");
    state_path.with_file_name(name)
}

/// Canonical role-labeled rendering of one session's body. This is the SINGLE
/// SOURCE OF TRUTH for what the input body hash covers and what Phase 4/5 feed
/// the distiller, so the hash a re-appearance compares against is exactly the
/// bytes the note was built from. A sub-agent turn is marked so a resume that
/// only re-runs a sub-agent still changes the hash.
pub fn canonical_body_text(messages: &[BodyMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        // `role`/`text` are defensively `Option` (future-malformed tolerance,
        // see `BodyMessage` docs); an absent field degrades to empty rather
        // than aborting the re-appearance hash.
        let role_text = msg.role.as_deref().unwrap_or("");
        let role = if msg.subagent {
            format!("{role_text}[subagent]")
        } else {
            role_text.to_string()
        };
        out.push_str(&role);
        out.push_str(": ");
        out.push_str(msg.text.as_deref().unwrap_or(""));
        out.push('\n');
    }
    out
}

/// The thread's canonical body: each member's canonical body concatenated in
/// the caller's (created) order, separated by an explicit member marker so two
/// different member splits can never hash identically.
pub fn thread_body_text(member_bodies: &[(String, Vec<BodyMessage>)]) -> String {
    let mut out = String::new();
    for (id, body) in member_bodies {
        out.push_str("=== session ");
        out.push_str(id);
        out.push_str(" ===\n");
        out.push_str(&canonical_body_text(body));
    }
    out
}

/// Hex SHA-256 of a canonical body text.
pub fn body_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The re-appearance decision for one thread, keyed by its primary session id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reappearance {
    /// Never-published id: an ordinary new note (Phase 5 publishes + records
    /// the first snapshot).
    NewNote,
    /// Published id that gained real content (hash changed) or `--force`: a
    /// follow-up note linking `prior`. Phase 5 publishes + records a new
    /// snapshot.
    FollowUp { prior: PublishedEntry },
    /// Published id with no material change. `snapshot_update` is `Some` when
    /// `n-msgs` grew but the body hash was unchanged: Phase 3 advances the
    /// stored `n-msgs` in place (note_path unchanged) so the deep check never
    /// re-runs. `None` when the cheap filter already matched (nothing to do).
    Skip { snapshot_update: Option<PublishedEntry> },
}

impl Reappearance {
    pub fn is_skip(&self) -> bool {
        matches!(self, Reappearance::Skip { .. })
    }
}

/// Cheap-filter step: does deciding this re-appearance require fetching the
/// body? Only when the id is published, `--force` is off, and `n-msgs` changed.
pub fn needs_body_fetch(prior: Option<&PublishedEntry>, current_total_msgs: i64, force: bool) -> bool {
    match prior {
        None => false,
        Some(_) if force => false,
        Some(entry) => current_total_msgs != entry.n_msgs,
    }
}

/// Finalize the re-appearance decision. `fresh_hash` is `Some` only when
/// [`needs_body_fetch`] returned true and the caller fetched+hashed the body.
pub fn classify_reappearance(
    prior: Option<&PublishedEntry>,
    current_total_msgs: i64,
    fresh_hash: Option<&str>,
    force: bool,
) -> Reappearance {
    log::debug!(
        "harvest::classify_reappearance: published={} current_msgs={} fresh_hash={} force={}",
        prior.is_some(),
        current_total_msgs,
        fresh_hash.is_some(),
        force
    );
    let Some(entry) = prior else {
        return Reappearance::NewNote;
    };
    if force {
        return Reappearance::FollowUp { prior: entry.clone() };
    }
    match fresh_hash {
        // No body was fetched -> the cheap filter matched (n-msgs unchanged).
        None => Reappearance::Skip { snapshot_update: None },
        Some(hash) if hash != entry.body_hash => Reappearance::FollowUp { prior: entry.clone() },
        Some(hash) => {
            // n-msgs grew but the body hash is unchanged: advance the snapshot
            // in place so the next run's cheap filter short-circuits. The
            // note/trace are UNCHANGED (this is not a new publish), so both
            // carry forward from the prior entry rather than being freshly
            // assigned.
            Reappearance::Skip {
                snapshot_update: Some(PublishedEntry {
                    note_path: entry.note_path.clone(),
                    n_msgs: current_total_msgs,
                    body_hash: hash.to_string(),
                    trace: entry.trace.clone(),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests;
