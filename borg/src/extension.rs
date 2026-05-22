use std::path::{Path, PathBuf};

use eyre::{Context, Result};

use crate::config::Config;

pub mod install;
pub mod manifest;
pub mod sign;

#[derive(Debug)]
pub struct GenerateResult {
    pub manifest_path: PathBuf,
    pub manifest_changed: bool,
}

#[derive(Debug)]
pub struct ValidateResult {
    pub manifest_drift: Option<String>,
}

impl ValidateResult {
    pub fn is_ok(&self) -> bool {
        self.manifest_drift.is_none()
    }
}

pub fn validate(repo_root: &Path, config: &Config) -> Result<ValidateResult> {
    log::debug!("extension::validate: repo_root={}", repo_root.display());
    let dir = extension_dir(repo_root);
    if !dir.exists() {
        eyre::bail!("extension directory not found: {}", dir.display());
    }

    let manifest_path = dir.join("manifest.json");
    let expected =
        serde_json::to_string_pretty(&manifest::build_manifest(config)).context("serialize manifest")? + "\n";
    let actual = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let manifest_drift = (expected != actual).then(|| describe_drift(&manifest_path, &actual, &expected));
    Ok(ValidateResult { manifest_drift })
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

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn extension_dir(repo_root: &Path) -> PathBuf {
    repo_root.join("borg").join("clients").join("extension")
}

pub fn generate(repo_root: &Path, config: &Config) -> Result<GenerateResult> {
    log::debug!("extension::generate: repo_root={}", repo_root.display());
    let dir = extension_dir(repo_root);
    if !dir.exists() {
        eyre::bail!("extension directory not found: {}", dir.display());
    }

    let new_content =
        serde_json::to_string_pretty(&manifest::build_manifest(config)).context("serialize manifest")? + "\n";
    let manifest_path = dir.join("manifest.json");
    let manifest_changed = match std::fs::read_to_string(&manifest_path) {
        Ok(existing) => existing != new_content,
        Err(_) => true,
    };
    if manifest_changed {
        std::fs::write(&manifest_path, &new_content).with_context(|| format!("write {}", manifest_path.display()))?;
    }
    Ok(GenerateResult {
        manifest_path,
        manifest_changed,
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
