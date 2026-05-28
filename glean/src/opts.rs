//! Opts: typed argument records the sb CLI layer passes to library
//! entry points. Mirrors the borg/cortex/oracle convention so sb's
//! `cli/glean.rs` does not import clap into the library boundary.

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct HarvestOpts {
    /// Re-classify every session even if jsonl_sha256 is unchanged.
    pub force: bool,
    /// Restrict harvest to one JSONL file (full path). Used by `sb glean
    /// show` and the daemon's per-event narrow harvest.
    pub only_jsonl: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct ClusterOpts {}

#[derive(Debug, Clone, Default)]
pub struct DistillOpts {
    /// Distill only one work-item by content_hash or slug.
    pub work_item: Option<String>,
    /// Re-distill even if the chunk file already exists at the
    /// expected content_hash.
    pub force: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DreamOpts {}

#[derive(Debug, Clone, Default)]
pub struct QuarantineOpts {
    pub action: QuarantineAction,
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub enum QuarantineAction {
    #[default]
    List,
    Inspect,
    Drop,
}

#[derive(Debug, Clone, Default)]
pub struct ShowOpts {
    /// Work-item content_hash (or slug) to print.
    pub work_item: String,
}

#[derive(Debug, Clone, Default)]
pub struct StatusOpts {}

#[derive(Debug, Clone, Default)]
pub struct DaemonOpts {
    pub install: bool,
    pub uninstall: bool,
    pub status: bool,
}
