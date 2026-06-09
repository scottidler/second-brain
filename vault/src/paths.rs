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

/// Subdirectory under `dirs::config_dir()` that owns every sb config file.
pub const SB_DIR: &str = "sb";

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

/// `~/.config/sb/` (or platform equivalent via `dirs`).
///
/// Panics only if `dirs::config_dir()` returns `None`, which on Linux
/// means both `$HOME` and `$XDG_CONFIG_HOME` are unset - a broken
/// environment where nothing else in sb would work either.
pub fn config_root() -> PathBuf {
    dirs::config_dir()
        .expect("dirs::config_dir() returned None (set HOME or XDG_CONFIG_HOME)")
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
        serde_yaml::from_str(&text).unwrap_or_default()
    }
}

/// Subdirectory under `dirs::data_local_dir()` that owns borg's
/// signal-rs linked-device state (Double Ratchet sessions, prekeys,
/// identity). One canonical path per borg installation; the operator
/// does NOT pick it. `signal-rs link --state-dir <path>` matches this
/// constant by convention.
pub const SB_BORG_SIGNAL_STATE_DIR: &str = "sb/borg/signal-state";

/// `~/.local/share/sb/borg/signal-state/` on Linux,
/// `~/Library/Application Support/sb/borg/signal-state/` on macOS.
/// Resolved at runtime via `dirs::data_local_dir()`.
///
/// Named `borg_signal_state_dir` (not `signal_state_dir`) so the
/// borg-scoped ownership is obvious in call sites — matches the
/// `SB_BORG_DATA_DIR` / `receipts_db_path` convention in
/// `vault::receipts`.
///
/// Panics only when `dirs::data_local_dir()` returns `None`, which
/// requires both `$HOME` and `$XDG_DATA_HOME` to be unset - a broken
/// environment where the rest of borg would also fail.
pub fn borg_signal_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .expect("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)")
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
/// when `dirs::data_local_dir()` returns `None` (see
/// [`borg_signal_state_dir`]).
pub fn borg_signal_bootstrap_marker() -> PathBuf {
    dirs::data_local_dir()
        .expect("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("sb/borg")
        .join("signal-bootstrap.json")
}

/// The single source of truth for the oracle SQLite DB path. Both oracle
/// (the reader / FTS5+vector indexer) and cortex (the sole embeddings
/// writer) resolve here so the two crates can never desync on the file
/// they open.
///
/// `~/.local/share/oracle/oracle.db` on Linux. Panics only when
/// `dirs::data_local_dir()` returns `None` (see [`borg_signal_state_dir`]).
pub fn oracle_db_path() -> PathBuf {
    dirs::data_local_dir()
        .expect("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)")
        .join("oracle")
        .join("oracle.db")
}

/// Resolve the vault root with explicit precedence: CLI > config > marker-gated CWD.
///
/// Returns an error rather than silently picking up an arbitrary working directory.
/// The CWD fallback only fires when the working directory contains a `.obsidian/`
/// directory - Obsidian writes this the moment a vault is opened in the app, so
/// this is the universal "this is a vault" signal.
pub fn resolve_vault_root(cli_override: Option<&Path>, config_value: Option<&str>) -> Result<PathBuf> {
    log::debug!(
        "resolve_vault_root: cli_override={:?} config_value={:?}",
        cli_override,
        config_value
    );
    if let Some(p) = cli_override {
        return Ok(p.to_path_buf());
    }
    if let Some(s) = config_value {
        let expanded = shellexpand::tilde(s);
        return Ok(PathBuf::from(expanded.as_ref()));
    }
    let cwd = std::env::current_dir()?;
    if cwd.join(".obsidian").is_dir() {
        return Ok(cwd);
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
