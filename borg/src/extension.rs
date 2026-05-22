use std::path::{Path, PathBuf};

use eyre::{Context, Result};

use crate::config::Config;

pub mod install;
pub mod manifest;
pub mod schema;
pub mod sign;

#[derive(Debug)]
pub struct GenerateResult {
    pub manifest_path: PathBuf,
    pub manifest_changed: bool,
    pub schema_path: PathBuf,
    pub schema_changed: bool,
}

#[derive(Debug)]
pub struct ValidateResult {
    pub manifest_drift: Option<String>,
    pub schema_drift: Option<String>,
}

impl ValidateResult {
    pub fn is_ok(&self) -> bool {
        self.manifest_drift.is_none() && self.schema_drift.is_none()
    }
}

pub fn validate(repo_root: &Path, config: &Config) -> Result<ValidateResult> {
    log::debug!("extension::validate: repo_root={}", repo_root.display());
    let dir = extension_dir(repo_root);
    if !dir.exists() {
        eyre::bail!("extension directory not found: {}", dir.display());
    }

    let manifest_path = dir.join("manifest.json");
    let manifest_expected_json = strip_volatile_fields(manifest::build_manifest(env!("CARGO_PKG_VERSION"), config));
    let manifest_actual_raw = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let manifest_actual_json: serde_json::Value =
        serde_json::from_str(&manifest_actual_raw).unwrap_or(serde_json::Value::Null);
    let manifest_actual_normalized = strip_volatile_fields(manifest_actual_json);
    let manifest_drift = (manifest_expected_json != manifest_actual_normalized).then(|| {
        let expected_text = serde_json::to_string_pretty(&manifest_expected_json).unwrap_or_default() + "\n";
        let actual_text = serde_json::to_string_pretty(&manifest_actual_normalized).unwrap_or_default() + "\n";
        describe_drift(&manifest_path, &actual_text, &expected_text)
    });

    let schema_path = dir.join("ingest-schema.json");
    let schema_expected = serde_json::to_string_pretty(&schema::build_schema()?).context("serialize schema")? + "\n";
    let schema_actual = std::fs::read_to_string(&schema_path).unwrap_or_default();
    let schema_drift =
        (schema_expected != schema_actual).then(|| describe_drift(&schema_path, &schema_actual, &schema_expected));

    Ok(ValidateResult {
        manifest_drift,
        schema_drift,
    })
}

/// Remove fields that change between regen calls without reflecting a real
/// structural change. Currently: the `version` field, which derives from
/// `env!("CARGO_PKG_VERSION")` and gets baked into the .xpi at sign time
/// regardless of what's committed. The committed value is informational
/// only - the live .xpi always reflects current Cargo.toml because
/// `sign::run` regenerates the manifest immediately before invoking
/// web-ext. Stripping it from validate means `bump` (which amends
/// Cargo.toml without regenerating manifest.json) does not produce false
/// drift on the tagged commit.
fn strip_volatile_fields(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("version");
    }
    value
}

fn describe_drift(path: &Path, actual: &str, expected: &str) -> String {
    let mut out = format!("--- a/{}\n+++ b/{} (regenerated)\n", path.display(), path.display());
    let actual_lines: Vec<&str> = actual.lines().collect();
    let expected_lines: Vec<&str> = expected.lines().collect();
    let max = actual_lines.len().max(expected_lines.len());
    let mut hits = 0usize;
    for i in 0..max {
        let a = actual_lines.get(i).copied().unwrap_or("");
        let e = expected_lines.get(i).copied().unwrap_or("");
        if a != e {
            if hits == 0 {
                out.push_str(&format!("@@ first mismatch at line {} @@\n", i + 1));
            }
            out.push_str(&format!("-{a}\n+{e}\n"));
            hits += 1;
            if hits >= MAX_DIFF_LINES {
                out.push_str("...(more lines omitted; run `sb borg extension generate` to regenerate)\n");
                break;
            }
        }
    }
    out
}

const MAX_DIFF_LINES: usize = 20;

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

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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

pub fn generate(repo_root: &Path, config: &Config) -> Result<GenerateResult> {
    log::debug!("extension::generate: repo_root={}", repo_root.display());
    let dir = extension_dir(repo_root);
    if !dir.exists() {
        eyre::bail!("extension directory not found: {}", dir.display());
    }

    let manifest_path = dir.join("manifest.json");
    let manifest_content = serde_json::to_string_pretty(&manifest::build_manifest(env!("CARGO_PKG_VERSION"), config))
        .context("serialize manifest")?
        + "\n";
    let manifest_changed = write_if_different(&manifest_path, &manifest_content)?;

    let schema_path = dir.join("ingest-schema.json");
    let schema_content = serde_json::to_string_pretty(&schema::build_schema()?).context("serialize schema")? + "\n";
    let schema_changed = write_if_different(&schema_path, &schema_content)?;

    Ok(GenerateResult {
        manifest_path,
        manifest_changed,
        schema_path,
        schema_changed,
    })
}

fn write_if_different(path: &Path, new_content: &str) -> Result<bool> {
    let changed = match std::fs::read_to_string(path) {
        Ok(existing) => existing != new_content,
        Err(_) => true,
    };
    if changed {
        std::fs::write(path, new_content).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(changed)
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
