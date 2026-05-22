use std::path::{Path, PathBuf};

use eyre::{Context, Result};

use crate::config::Config;

pub mod install;
pub mod manifest;
pub mod schema;
pub mod sign;

#[derive(Debug)]
pub struct SignResult {
    pub extension_dir: PathBuf,
    pub xpi_path: PathBuf,
    pub version: String,
}

#[derive(Debug)]
pub struct StageResult {
    pub target_dir: PathBuf,
}

pub fn extension_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("borg").join("clients").join("extension")
}

/// Materialise the full Firefox extension (manifest + schema + static
/// assets + AMO sidecar) into `target_dir`. The `version` parameter is
/// stamped into the manifest's `version` field; callers (sb's CLI)
/// pass `env!("CARGO_PKG_VERSION")` from the sb crate so the .xpi
/// reflects the binary's identity.
///
/// Idempotent overwrite of any existing files. Used by `sign::run`
/// (with a TempDir) and `sb borg extension stage --to <DIR>` (with a
/// user-supplied directory).
pub fn stage(target_dir: &Path, version: &str, config: &Config) -> Result<StageResult> {
    log::debug!(
        "extension::stage: target_dir={} version={}",
        target_dir.display(),
        version
    );
    let source_dir = repo_root()?;
    let source_ext_dir = extension_dir(&source_dir);
    if !source_ext_dir.exists() {
        eyre::bail!("extension source directory not found: {}", source_ext_dir.display());
    }
    std::fs::create_dir_all(target_dir.join("icons")).context("create target icons dir")?;

    let static_files: &[&str] = &[
        "background.js",
        "options.html",
        "options.js",
        "icons/locutus-16.png",
        "icons/locutus-48.png",
        "icons/locutus-128.png",
        ".amo-upload-uuid",
    ];
    for rel in static_files {
        let src = source_ext_dir.join(rel);
        let dst = target_dir.join(rel);
        std::fs::copy(&src, &dst).with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    }

    let manifest_content =
        serde_json::to_string_pretty(&manifest::build_manifest(version, config)).context("serialize manifest")? + "\n";
    std::fs::write(target_dir.join("manifest.json"), manifest_content).context("write manifest.json")?;

    let schema_content = serde_json::to_string_pretty(&schema::build_schema()?).context("serialize schema")? + "\n";
    std::fs::write(target_dir.join("ingest-schema.json"), schema_content).context("write ingest-schema.json")?;

    Ok(StageResult {
        target_dir: target_dir.to_path_buf(),
    })
}

pub fn repo_root() -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git")?;
    if !output.status.success() {
        eyre::bail!("Not inside a git repository - cannot locate extension directory");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}
