//! Shared path resolution for the unified `sb` binary.
//!
//! Every loader, bootstrap site, doctor site, and systemd unit template
//! reads from this module. Hardcoded paths anywhere else are a
//! code-review reject.
//!
//! On-disk layout:
//!
//! ```text
//! ~/.config/sb/
//!   borg.yml
//!   cortex.yml
//!   oracle.yml
//!   canonical-tags.yml
//!   tag-mapping.yml
//!   tag-proposals.yml
//!   patterns/
//!     distill-article.md
//!     ...
//! ```
//!
//! Logs sit symmetrically under `~/.local/share/sb/` (see `vault::logging`).

use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use serde::{Deserialize, Deserializer};
use walkdir::WalkDir;

/// Subdirectory under `xdg_config_dir()` that owns every sb config file.
pub const SB_DIR: &str = "sb";

/// XDG config dir, honoring `$XDG_CONFIG_HOME` and falling back to `$HOME/.config`.
/// We deliberately do NOT use `dirs::config_dir()`: it honors `$XDG_CONFIG_HOME` only on
/// Linux; on macOS it returns `~/Library/Application Support`, ignoring the env var.
pub fn xdg_config_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|h| h.join(".config"))
}

/// XDG data dir, honoring `$XDG_DATA_HOME` and falling back to `$HOME/.local/share`.
/// Same rationale as `xdg_config_dir`: `dirs::data_local_dir()` returns `~/Library/...`
/// on macOS, ignoring `$XDG_DATA_HOME`. This resolves to the XDG layout on every platform.
pub fn xdg_data_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        let path = PathBuf::from(dir);
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|h| h.join(".local").join("share"))
}

/// Expand a leading `~` or `~/` in a user-supplied path to `$HOME`.
///
/// Users routinely type `~/foo` in YAML config; YAML stores that as the
/// literal three characters. If we hand the literal to `fs::create_dir_all`
/// or any other filesystem call, a directory literally named `~` is created
/// in the process's CWD. Every `PathBuf` field that originates from user
/// config must pass through this (or a serde wrapper that calls it) before
/// it reaches the filesystem.
///
/// Non-tilde paths pass through untouched.
pub fn expand_tilde(path: impl AsRef<Path>) -> PathBuf {
    let s = path.as_ref().to_string_lossy();
    PathBuf::from(shellexpand::tilde(s.as_ref()).as_ref())
}

/// `#[serde(deserialize_with = "vault::paths::deserialize_tilde_pathbuf")]`
/// for `PathBuf` config fields. Runs the deserialized value through
/// [`expand_tilde`] so a literal `~/...` in YAML becomes a real absolute
/// path the moment the config loads.
pub fn deserialize_tilde_pathbuf<'de, D>(deserializer: D) -> std::result::Result<PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = PathBuf::deserialize(deserializer)?;
    Ok(expand_tilde(raw))
}

/// Sum the size in bytes of every regular file under `root`, recursing into
/// subdirectories. Does not follow symlinks (a cycle would hang `sb doctor`;
/// a symlinked file's target size would double-count storage that isn't
/// actually inside `root`). A missing or unreadable `root` (and any
/// unreadable entry within it) contributes 0, not an error - this is a
/// doctor Info/Warn signal, not a build-breaking check.
pub fn dir_size(root: &Path) -> u64 {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// `~/.config/sb/` (XDG on every platform via [`xdg_config_dir`]).
///
/// Panics only if `xdg_config_dir()` returns `None`, which
/// means both `$HOME` and `$XDG_CONFIG_HOME` are unset - a broken
/// environment where nothing else in sb would work either.
pub fn config_root() -> PathBuf {
    xdg_config_dir()
        .expect("xdg_config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
        .join(SB_DIR)
}

pub fn borg_config() -> PathBuf {
    config_root().join("borg.yml")
}

pub fn cortex_config() -> PathBuf {
    config_root().join("cortex.yml")
}

pub fn oracle_config() -> PathBuf {
    config_root().join("oracle.yml")
}

pub fn canonical_tags() -> PathBuf {
    config_root().join("canonical-tags.yml")
}

pub fn tag_mapping() -> PathBuf {
    config_root().join("tag-mapping.yml")
}

pub fn tag_proposals() -> PathBuf {
    config_root().join("tag-proposals.yml")
}

/// Concept glossary + alias table used by `cortex link` (Phase 2 of the
/// graph-augmented-memory design). Kebab-case concept slugs mirror
/// `canonical-tags.yml`; the `aliases` block maps surface forms to slugs.
pub fn glossary() -> PathBuf {
    config_root().join("glossary.yml")
}

/// LLM-proposed glossary entries awaiting human promotion (Phase 4 of the
/// graph-augmented-memory design), mirroring `tag-proposals.yml`.
pub fn entity_proposals() -> PathBuf {
    config_root().join("entity-proposals.yml")
}

/// LLM-proposed cross-repo bridges awaiting human approval (harvest-completion
/// design, Phase 7 historical multi-repo backfill), mirroring
/// `entity-proposals.yml`. Each proposal adds a `[[member]]` wikilink to a
/// secondary repo hub's body; never applied silently.
pub fn bridge_proposals() -> PathBuf {
    config_root().join("bridge-proposals.yml")
}

pub fn patterns_dir() -> PathBuf {
    config_root().join("patterns")
}

pub fn cli_config() -> PathBuf {
    config_root().join("cli.yml")
}

/// CLI ergonomics config for short-lived sb invocations.
///
/// Lives at `~/.config/sb/cli.yml`. All fields optional; missing file falls
/// back to defaults. Schema:
///
/// ```yaml
/// logging:
///   level: debug         # log level for sb CLI invocations (not the daemon)
///   status: true         # per-verb: do CLI verbs write to the subsystem log file?
///   doctor: false
///   bootstrap: true
///   borg-daemon-status: false
///   # verbs not listed default to false (stderr only)
/// ```
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct CliConfig {
    pub logging: CliLogging,
}

/// YAML shape mirrors the CLI command tree exactly:
///
/// ```yaml
/// logging:
///   level: debug              # log level for sb CLI processes
///   status: true              # sb status
///   doctor: true              # sb doctor
///   bootstrap: true           # sb bootstrap
///   borg:
///     daemon:
///       status: true          # sb borg daemon --status
///       install: true         # sb borg daemon --install
///     log: true               # sb borg log
///   cortex:
///     daemon:
///       status: true          # sb cortex daemon --status
///   oracle:
///     stats: true             # sb oracle stats
/// ```
///
/// Verbs absent from the file default to `false` (stderr-only). The
/// command hierarchy under `logging:` is held as an opaque YAML
/// mapping; lookup is by path slice (see [`CliLogging::opted_in`]).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct CliLogging {
    /// Log level for short-lived sb CLI processes. Daemon log level stays
    /// in `borg.yml` / `cortex.yml`. Resolution order in `sb`:
    /// `--log-level` flag > `--verbose` > this field > `"info"`.
    pub level: Option<String>,
    /// Everything else under `logging:` is the verb-opt-in tree. Stored
    /// as a raw mapping so we don't have to mirror the CLI command tree
    /// with one struct per node. Walk it with `opted_in(&["borg", "log"])`.
    #[serde(flatten)]
    pub verbs: serde_yaml::Mapping,
}

impl CliLogging {
    /// `true` iff the YAML path resolves to a literal `true` leaf.
    /// Missing keys, missing intermediate nodes, and non-bool leaves all
    /// return `false` so a default-empty config is the safe state.
    pub fn opted_in(&self, path: &[&str]) -> bool {
        let mut current = &self.verbs;
        for (i, segment) in path.iter().enumerate() {
            let Some(value) = current.get(*segment) else {
                return false;
            };
            if i + 1 == path.len() {
                return matches!(value, serde_yaml::Value::Bool(true));
            }
            let serde_yaml::Value::Mapping(m) = value else {
                return false;
            };
            current = m;
        }
        false
    }
}

impl CliConfig {
    /// Load `~/.config/sb/cli.yml`. Missing file or parse failure returns
    /// defaults (so a freshly bootstrapped machine does not need the file).
    pub fn load() -> Self {
        let path = cli_config();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_yaml::from_str(&text) {
            Ok(config) => config,
            Err(e) => {
                log::warn!(
                    "CliConfig::load: {} failed to parse, using defaults: {e}",
                    path.display()
                );
                Self::default()
            }
        }
    }
}

/// Subdirectory under `xdg_data_dir()` that owns borg's
/// signal-rs linked-device state (Double Ratchet sessions, prekeys,
/// identity). One canonical path per borg installation; the operator
/// does NOT pick it. `signal-rs link --state-dir <path>` matches this
/// constant by convention.
pub const SB_BORG_SIGNAL_STATE_DIR: &str = "sb/borg/signal-state";

/// `~/.local/share/sb/borg/signal-state/` on Linux,
/// `~/Library/Application Support/sb/borg/signal-state/` on macOS.
/// Resolved at runtime via `xdg_data_dir()`.
///
/// Named `borg_signal_state_dir` (not `signal_state_dir`) so the
/// borg-scoped ownership is obvious in call sites — matches the
/// `SB_BORG_DATA_DIR` / `receipts_db_path` convention in
/// `vault::receipts`.
///
/// Panics only when `xdg_data_dir()` returns `None`, which
/// requires both `$HOME` and `$XDG_DATA_HOME` to be unset - a broken
/// environment where the rest of borg would also fail.
pub fn borg_signal_state_dir() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join(SB_BORG_SIGNAL_STATE_DIR)
}

/// borg-owned marker recording the last successful Signal cold-start
/// bootstrap self-send (see
/// `docs/design/2026-05-28-signal-cold-start-bootstrap.md`). Lives
/// directly under `sb/borg/` - deliberately OUTSIDE the
/// signal-rs-owned `signal-state/` dir so it never collides with the
/// `store.db` signal-rs manages there.
///
/// `~/.local/share/sb/borg/signal-bootstrap.json` on Linux. Panics only
/// when `xdg_data_dir()` returns `None` (see
/// [`borg_signal_state_dir`]).
pub fn borg_signal_bootstrap_marker() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/borg")
        .join("signal-bootstrap.json")
}

/// Default root for borg's per-trace staging directories, under the
/// `sb/borg/` data namespace. Each ingest's staged artifacts (`fetched.html`,
/// `transcript.md`, `distilled.yml`) live at `<this>/<trace_id>/`. borg's
/// `StagingConfig.root` defaults to this, and cortex's `embed.staging-root`
/// defaults to the same value so the two subsystems resolve the identical
/// path without hardcoding it twice: cortex reads the staged `distilled.yml`
/// (read-only) as the transcript-embedding source for Video/Article notes
/// (2026-07-07-distillation-output-restore Phase 5). borg remains the sole
/// staging WRITER.
///
/// `~/.local/share/sb/borg/stages/` on Linux. Panics only when
/// `xdg_data_dir()` returns `None` (see [`borg_signal_state_dir`]).
pub fn borg_stages_dir() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/borg")
        .join("stages")
}

/// cortex's embed/graph file-lock path, under the `sb/cortex/` data
/// namespace. The lock serializes the embed and graph passes (they share
/// it). Lives under `sb/cortex/` like borg's data, NOT the legacy
/// `~/.local/share/cortex/` (outside the `sb/` namespace). The lock is
/// ephemeral, so relocating it needs no migration.
///
/// `~/.local/share/sb/cortex/embed.lock` on Linux. Panics only when
/// `xdg_data_dir()` returns `None` (see [`borg_signal_state_dir`]).
pub fn cortex_lock_path() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/cortex")
        .join("embed.lock")
}

/// `sb borg eval`'s distillation judgment-cache path, under the `sb/borg/`
/// data namespace (per-host, beside the receipts DB). Keyed judgments persist
/// here so a re-run is cache-hit stable. Panics only when `xdg_data_dir()`
/// returns `None`, same as [`borg_signal_state_dir`].
///
/// `~/.local/share/sb/borg/eval-cache.db` on Linux.
pub fn borg_eval_cache_path() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/borg")
        .join("eval-cache.db")
}

/// `sb borg harvest`'s watermark + durable-identity state file, under the
/// `sb/borg/` data namespace (per-host, beside the receipts DB). Holds the
/// export cursor plus, per published session id, the note path, `n-msgs` at
/// publish, and the input-body hash (harvest-clyde-sessions design,
/// Architecture > Watermark + durable identity). The harvest job takes an
/// exclusive lock on this file so a nightly timer run and a hand-run cannot
/// race the cursor. Panics only when `xdg_data_dir()` returns `None`, same as
/// [`borg_signal_state_dir`].
///
/// `~/.local/share/sb/borg/harvest-state.json` on Linux.
pub fn borg_harvest_state() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/borg")
        .join("harvest-state.json")
}

/// The single source of truth for the oracle SQLite DB path. Both oracle
/// (the reader / FTS5+vector indexer) and cortex (the sole embeddings
/// writer) resolve here so the two crates can never desync on the file
/// they open.
///
/// `~/.local/share/oracle/oracle.db` on Linux. Panics only when
/// `xdg_data_dir()` returns `None` (see [`borg_signal_state_dir`]).
pub fn oracle_db_path() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("oracle")
        .join("oracle.db")
}

/// `sb oracle eval`'s judgment-cache path, beside the oracle DB in the data
/// dir. Used as the fallback when the configured DB path has no parent; never
/// a relative `eval-cache.db` (which would write under CWD). Panics only when
/// `xdg_data_dir()` returns `None`, same as [`oracle_db_path`].
pub fn oracle_eval_cache_path() -> PathBuf {
    xdg_data_dir()
        .expect("xdg_data_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("oracle")
        .join("eval-cache.db")
}

/// Resolve the vault root with explicit precedence: CLI > config > marker-gated CWD.
///
/// Returns an error rather than silently picking up an arbitrary working directory.
/// The CWD fallback only fires when the working directory contains a `.obsidian/`
/// directory - Obsidian writes this the moment a vault is opened in the app, so
/// this is the universal "this is a vault" signal.
/// Refuse a vault root that is the second-brain workspace itself.
///
/// cortex governs a vault by REWRITING it: lint fixes frontmatter, the naming
/// rule renames files, autotag and link edit bodies. Pointed at this repo it
/// treats source as notes. On 2026-08-15 that happened for real (a `sb bootstrap`
/// run from inside the checkout baked `--vault <repo>` into the systemd unit) and
/// cortex rewrote 203 files: every `borg/patterns/*.md` prompt gained note
/// frontmatter and wikilinks, the `config/eval/distill-fixtures/**` goldens were
/// edited, and the naming lint renamed `AGENTS.md` to `agents.md`.
///
/// The check is structural, not name-based, so a checkout under any directory
/// name is caught: a Cargo manifest sitting next to this workspace's own member
/// crates. A real Obsidian vault has neither.
fn reject_self_repo(root: PathBuf) -> Result<PathBuf> {
    let looks_like_this_workspace = root.join("Cargo.toml").is_file()
        && root.join("cortex").is_dir()
        && root.join("borg").is_dir()
        && root.join("vault").is_dir();
    if looks_like_this_workspace {
        return Err(eyre!(
            "refusing to use the second-brain source tree as a vault root: {}\n\
             cortex REWRITES what it governs (lint renames files, autotag and link edit bodies), \
             so pointing it here corrupts patterns, eval fixtures, and docs.\n\
             Set `vault.root-path` in your config, or pass --vault <your Obsidian vault>.",
            root.display()
        ));
    }
    Ok(root)
}

pub fn resolve_vault_root(cli_override: Option<&Path>, config_value: Option<&str>) -> Result<PathBuf> {
    log::debug!(
        "resolve_vault_root: cli_override={:?} config_value={:?}",
        cli_override,
        config_value
    );
    if let Some(p) = cli_override {
        return reject_self_repo(p.to_path_buf());
    }
    if let Some(s) = config_value {
        let expanded = shellexpand::tilde(s);
        return reject_self_repo(PathBuf::from(expanded.as_ref()));
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".obsidian").is_dir() {
        return reject_self_repo(cwd);
    }
    Err(eyre!(
        "vault root not set: pass --vault <path>, set `vault.root-path` in your config, \
         or run from a directory that contains a `.obsidian/` directory.\n\
         (current directory: {})",
        cwd.display()
    ))
}

#[cfg(test)]
mod tests;
