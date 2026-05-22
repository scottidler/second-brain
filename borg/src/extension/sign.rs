use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{Context, Result};

use crate::config::Config;
use crate::extension::{self, SignResult};

pub fn run(repo_root: &Path, config: &Config) -> Result<SignResult> {
    log::debug!("extension::sign::run: repo_root={}", repo_root.display());
    let dir = extension::extension_dir(repo_root);
    if !dir.exists() {
        eyre::bail!("Extension directory not found at {}", dir.display());
    }

    extension::generate(repo_root, config).context("regenerate manifest before sign")?;

    let cargo_version = env!("CARGO_PKG_VERSION");
    let jwt_issuer =
        std::env::var("MOZILLA_JWT_ISSUER").context("MOZILLA_JWT_ISSUER env var must be set (AMO API key)")?;
    let jwt_secret =
        std::env::var("MOZILLA_JWT_SECRET").context("MOZILLA_JWT_SECRET env var must be set (AMO API secret)")?;

    log::info!("signing extension v{cargo_version} in {}", dir.display());

    let status = Command::new("web-ext")
        .args([
            "sign",
            "--api-key",
            &jwt_issuer,
            "--api-secret",
            &jwt_secret,
            "--channel",
            "unlisted",
        ])
        .current_dir(&dir)
        .status()
        .context("Failed to run web-ext - is it installed?")?;

    if !status.success() {
        eyre::bail!("web-ext sign failed with exit {status}");
    }

    let artifacts_dir = dir.join("web-ext-artifacts");
    let xpi_path = locate_versioned_xpi(&artifacts_dir, cargo_version)?;

    Ok(SignResult {
        extension_dir: dir,
        xpi_path,
        version: cargo_version.to_string(),
    })
}

fn locate_versioned_xpi(artifacts_dir: &Path, version: &str) -> Result<PathBuf> {
    let entries = std::fs::read_dir(artifacts_dir).with_context(|| format!("reading {}", artifacts_dir.display()))?;
    let mut matches: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            name.ends_with(".xpi") && name.contains(version)
        })
        .collect();
    matches.sort();
    match matches.len() {
        0 => eyre::bail!(
            "web-ext sign produced no .xpi for version {version} in {}",
            artifacts_dir.display()
        ),
        1 => Ok(matches.remove(0)),
        n => eyre::bail!("expected exactly 1 .xpi for version {version}, found {n}: {matches:?}"),
    }
}
