# Design Document: Shared Vault Watcher + Oracle Live Reindex

**Author:** Scott Idler
**Date:** 2026-03-22
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Extract cortex's debounced file watcher into the shared `vault` crate as a reusable `VaultWatcher`, then use it in oracle to trigger automatic reindex when notes change on disk. Cortex adopts the same shared implementation, eliminating duplicate watcher code.

## Problem Statement

### Background

Oracle's SQLite search index only updates on two occasions: server startup and manual `reindex` MCP tool calls. Between those events, the index is stale - newly ingested or promoted notes are invisible to search until someone remembers to call reindex.

Cortex already has a debounced file watcher (`cortex/src/daemon.rs`) using the `notify` crate. Oracle needs the same capability, but duplicating the watcher + debounce + event filtering logic is wasteful when both binaries share the `vault` library crate.

### Problem

1. Oracle's search index goes stale between startup and manual reindex calls
2. The watcher/debounce pattern in cortex is not reusable - it's embedded in cortex's daemon loop
3. The vault has a specific event pattern that causes thrashing: borg writes to `inbox/`, cortex promotes to `notes/` seconds later - two filesystem events for one logical change

### Goals

- Oracle search index stays current without manual intervention
- Shared `VaultWatcher` in the `vault` crate, usable by both cortex and oracle
- Debounce collapses rapid event sequences (borg + cortex) into a single callback
- Cortex migrates to the shared watcher, reducing its daemon.rs complexity

### Non-Goals

- Real-time indexing (sub-second latency) - debounce inherently adds a delay
- Watching non-vault directories
- Replacing cortex's full daemon loop (sweep scheduling, intel timers, cycle detection stay in cortex)

## Proposed Solution

### Overview

Add a `VaultWatcher` to the `vault` crate behind a new `watcher` feature flag. It wraps `notify` + `tokio` to provide: recursive vault watching, `.md`-only filtering, directory ignore lists, and a configurable debounce that sends batched changed paths through a `tokio::mpsc` channel.

Cortex replaces its inline watcher setup with `VaultWatcher`. Oracle spawns a `VaultWatcher` alongside the MCP server and calls `index_vault()` when the channel fires.

### Architecture

```
vault (library crate)
  src/watcher.rs       <-- NEW: VaultWatcher, WatcherConfig
  Cargo.toml           <-- new "watcher" feature flag

cortex (binary)
  daemon.rs            <-- replace inline notify setup with VaultWatcher
  Cargo.toml           <-- vault = { features = ["watcher"] }

oracle (binary)
  main.rs              <-- spawn VaultWatcher task in run_serve()
  Cargo.toml           <-- vault = { features = ["watcher"] }
```

### Data Model

```rust
// vault/src/watcher.rs

/// Configuration for the vault file watcher
pub struct WatcherConfig {
    /// Seconds to wait after last event before firing (default: 5)
    pub debounce_secs: u64,
    /// Directory names to ignore (e.g., [".git", ".obsidian", "templates"])
    pub ignore_dirs: Vec<String>,
}

/// A debounced filesystem watcher for an Obsidian vault.
/// Watches recursively, filters to .md files, debounces rapid changes,
/// and sends batched paths through a channel.
pub struct VaultWatcher {
    watcher: RecommendedWatcher,
}

/// Events emitted by the watcher after debounce
pub struct VaultChange {
    pub changed_paths: Vec<PathBuf>,
}
```

### API Design

```rust
// vault/src/watcher.rs

impl VaultWatcher {
    /// Start watching a vault root directory.
    /// Returns the watcher handle and a receiver for debounced change events.
    ///
    /// Internally spawns a tokio task that collects raw notify events,
    /// filters them, and runs the debounce timer. The receiver emits
    /// VaultChange batches only after the debounce window closes.
    ///
    /// The optional `applying` flag lets callers suppress events during
    /// their own vault writes (prevents feedback loops). Pass None if
    /// the consumer is read-only (e.g., oracle).
    pub fn start(
        vault_root: &Path,
        config: WatcherConfig,
        applying: Option<Arc<AtomicBool>>,
    ) -> Result<(Self, tokio::sync::mpsc::UnboundedReceiver<VaultChange>)>;
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_secs: 5,
            ignore_dirs: vec![
                ".git".into(),
                ".obsidian".into(),
                "templates".into(),
            ],
        }
    }
}
```

### Internal Mechanics

`VaultWatcher::start()` does three things:

1. Creates a `notify::RecommendedWatcher` with a sync callback that sends raw events into a `tokio::sync::mpsc::unbounded_channel`
2. Spawns a tokio task that runs the debounce loop (ported from cortex's `daemon.rs:139-172`):
   - Receives raw events from the notify channel
   - Filters: only Create/Modify/Remove events, only `.md` files, skip ignore dirs
   - If `applying` flag is set, discard events silently
   - Accumulates changed paths in a `Vec<PathBuf>`, resets debounce timer on each event
   - When debounce timer fires and pending paths exist, sends a `VaultChange` to the output channel and clears the accumulator
3. Returns the watcher handle and the output receiver. The handle must be held alive for the duration of watching - dropping it stops the notify watcher and the debounce task exits when its input channel closes

The consumer just `rx.recv().await` in a loop - all filtering and debounce is handled internally.

### Consumer: Oracle

```rust
// oracle/src/main.rs - run_serve()

async fn run_serve(config: Config) -> Result<()> {
    let db = SearchIndex::open(&config.db_path())?;
    db.index_vault(&config.vault_root())?;  // startup index (unchanged)

    let server = OracleMcpServer::new(config.clone(), db);

    // Spawn file watcher for live reindex (oracle is read-only, no applying flag)
    let (watcher, mut rx) = VaultWatcher::start(
        &config.vault_root(),
        WatcherConfig::default(),
        None,  // no applying flag - oracle never writes to the vault
    )?;

    let db_handle = server.db_handle();  // Arc<Mutex<SearchIndex>>
    let vault_root = config.vault_root();
    tokio::spawn(async move {
        while let Some(change) = rx.recv().await {
            tracing::info!(
                "vault changed ({} files), reindexing",
                change.changed_paths.len()
            );
            if let Ok(db) = db_handle.lock() {
                match db.index_vault(&vault_root) {
                    Ok(stats) => tracing::info!(
                        "reindex: {} updated, {} inserted",
                        stats.updated, stats.inserted
                    ),
                    Err(e) => tracing::warn!("reindex failed: {e}"),
                }
            }
        }
    });

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}
```

### Consumer: Cortex

Cortex's `start_watching()` replaces ~30 lines of inline notify setup with:

```rust
let (watcher, mut watch_rx) = VaultWatcher::start(
    vault_root,
    WatcherConfig {
        debounce_secs: daemon_config.debounce_secs,
        ignore_dirs: config.vault.ignore.clone(),
    },
    Some(Arc::clone(&applying)),  // cortex writes to vault, suppress during apply
)?;
```

The rest of cortex's daemon loop (sweep scheduling, intel timers, cycle detection, action dispatch) stays in cortex. Only the raw watcher + debounce + event filtering moves to vault.

### Implementation Plan

**Phase 1: Extract VaultWatcher into vault crate**
- Add `watcher` feature flag to `vault/Cargo.toml` (depends on `notify`, `tokio`)
- Create `vault/src/watcher.rs` with `VaultWatcher`, `WatcherConfig`, `VaultChange`
- Port the debounce + filtering logic from `cortex/src/daemon.rs`
- Expose behind the feature flag in `vault/src/lib.rs`

**Phase 2: Oracle integration**
- Add `watcher` feature to oracle's vault dependency
- Add `tokio` with `rt` feature (already present)
- Spawn watcher task in `run_serve()`, reindex on change events
- Expose `db_handle()` on `OracleMcpServer` for the spawned task

**Phase 3: Cortex migration**
- Add `watcher` feature to cortex's vault dependency
- Replace inline watcher setup in `daemon.rs` with `VaultWatcher::start()`
- Remove `notify` direct dependency from cortex (it comes through vault)
- Verify daemon behavior is unchanged

## Alternatives Considered

### Alternative 1: Duplicate watcher in oracle
- **Description:** Copy cortex's watcher pattern into oracle directly
- **Pros:** Fast to implement, no cross-crate changes
- **Cons:** Two copies of the same logic to maintain, bug fixes need applying twice
- **Why not chosen:** Violates the workspace's shared-library principle

### Alternative 2: Periodic timer instead of file watcher
- **Description:** Reindex every N seconds on a timer
- **Pros:** Simpler, no notify dependency
- **Cons:** Either too frequent (wastes CPU) or too infrequent (stale index), no debounce benefit
- **Why not chosen:** File watcher is more efficient and responsive

### Alternative 3: Cortex notifies oracle via IPC
- **Description:** After cortex promotes a note, it signals oracle to reindex
- **Pros:** Precise - only reindexes when cortex is done
- **Cons:** Tight coupling between daemons, doesn't catch manual edits or borg-only changes, adds IPC complexity
- **Why not chosen:** File watcher is simpler and catches all change sources

## Technical Considerations

### Dependencies

New dependencies for the `vault` crate (behind `watcher` feature):
- `notify` - filesystem watcher (already used by cortex, version 7.0)
- `tokio` - async runtime, mpsc channel, timers (already a workspace dep)

### Performance

- `index_vault` is incremental (mtime comparison) - a reindex after 2-3 file changes touches only those files, not the full 900+ note vault
- The 5-second debounce means at most ~12 reindexes per minute under sustained load
- Typical borg ingest + cortex promote: one reindex per ingest (the two events collapse into one debounce window)
- Batch reingest of 150 items: events accumulate during the flurry, debounce fires once things settle, single reindex picks up everything

### Thrash Analysis

The specific borg-then-cortex pattern:
1. **t=0s:** borg writes `inbox/foo.md` - watcher event, debounce timer starts (5s)
2. **t=2s:** cortex moves `inbox/foo.md` to `notes/foo.md` - watcher event, debounce timer resets (5s from now)
3. **t=7s:** debounce fires, oracle reindexes once - sees `notes/foo.md` at its final location

No thrashing. The debounce naturally absorbs the two-step dance.

### Testing Strategy

- Unit tests for `VaultWatcher`: create temp dir, write files, verify debounced events arrive with correct paths
- Unit test for event filtering: verify ignore dirs are skipped, non-.md files are skipped
- Integration test: write a file, wait for debounce, verify index updated (oracle-level)
- Manual test: run borg ingest with oracle active, verify note is searchable within ~10 seconds

### Rollout Plan

Ship all three phases in a single version bump. Cortex and oracle are co-deployed (same systemd user), so there's no compatibility concern. The watcher feature is additive - existing oracle behavior (startup index + manual reindex) is preserved.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| notify inotify watch limit on large vaults | Low | Med | Linux default is 8192 watches, vault has ~1000 files. Document `fs.inotify.max_user_watches` if needed |
| Watcher thread panics silently | Low | Med | Log errors from the notify callback, oracle continues serving with stale index |
| Reindex blocks MCP responses (Mutex) | Med | Low | index_vault is fast for incremental updates (~50ms for 2-3 files). Could move to RwLock if contention appears |
| Oracle stdio transport + tokio::spawn conflicts | Low | Med | rmcp already runs on tokio, spawned task is independent |

## Resolved Questions

- **Watcher config location:** Oracle gets a `watcher` section in `oracle.yml` with `debounce-secs` and `ignore` fields, matching cortex's `daemon` config pattern. Defaults are sensible (5s debounce, standard ignore dirs).
- **Logging crate:** `VaultWatcher` uses the `log` crate, consistent with the rest of vault. Oracle's `tracing-subscriber` already captures `log` events via the compatibility layer - no issue.
- **Applying flag:** Made `Option<Arc<AtomicBool>>` - oracle passes `None` (read-only), cortex passes `Some(...)` (writes to vault during apply).

## References

- Cortex daemon watcher implementation: `cortex/src/daemon.rs:52-172`
- Oracle MCP server: `oracle/src/server.rs`, `oracle/src/main.rs`
- Vault SearchIndex incremental indexing: `vault/src/search.rs:179-235`
- notify crate: https://docs.rs/notify
