use std::path::{Path, PathBuf};
use std::process::Command;

use std::fmt;

use eyre::{Context, ContextCompat, Result};

use crate::config::Config;
use crate::extension::{self, SignResult};

/// The reuse path found a signed `.xpi` whose baked config no longer matches
/// this host's `borg.yml` (or has no sidecar to compare). AMO holds the
/// version already, so the only re-sign is a version bump. `sb borg extension
/// install` maps this to exit 3.
#[derive(Debug)]
pub struct ConfigMismatch {
    pub version: String,
    pub detail: &'static str,
}

impl fmt::Display for ConfigMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "extension v{} {}; bump the version to re-sign",
            self.version, self.detail
        )
    }
}

impl std::error::Error for ConfigMismatch {}

fn sidecar_path(artifacts_dir: &Path, version: &str) -> PathBuf {
    artifacts_dir.join(format!("{version}.config-hash"))
}

fn check_sidecar(artifacts_dir: &Path, version: &str, current_hash: &str) -> Result<()> {
    let path = sidecar_path(artifacts_dir, version);
    let recorded = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigMismatch {
                version: version.to_string(),
                detail: "has a signed .xpi but no config-hash sidecar, so its baked config is unknown",
            }
            .into());
        }
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    if recorded.trim() != current_hash {
        return Err(ConfigMismatch {
            version: version.to_string(),
            detail: "was signed with different extension config",
        }
        .into());
    }
    Ok(())
}

pub fn run(repo_root: &Path, config: &Config, version: &str) -> Result<SignResult> {
    log::debug!(
        "extension::sign::run: repo_root={} version={}",
        repo_root.display(),
        version
    );
    let source_dir = extension::extension_dir(repo_root);
    if !source_dir.exists() {
        eyre::bail!("Extension directory not found at {}", source_dir.display());
    }

    let artifacts_dir = source_dir.join("web-ext-artifacts");
    std::fs::create_dir_all(&artifacts_dir)
        .with_context(|| format!("create artifacts dir {}", artifacts_dir.display()))?;

    // Idempotency: AMO is a one-publish-per-version service. If an .xpi for
    // this version already exists in the artifacts dir (signed earlier on
    // this machine, or transported from another), reuse it. A second sign
    // attempt always returns `Version X.Y.Z already exists` from AMO and
    // exits non-zero, which breaks multi-machine deploys and any retry of a
    // single-machine deploy.
    let config_hash = extension::daemon::baked_config_hash(config).context("hash baked extension config")?;
    if let Ok(existing) = locate_versioned_xpi(&artifacts_dir, version) {
        check_sidecar(&artifacts_dir, version, &config_hash)?;
        log::info!(
            "extension::sign::run: reusing existing signed .xpi for v{version} ({}); skipping AMO upload",
            existing.display()
        );
        return Ok(SignResult {
            extension_dir: source_dir,
            xpi_path: existing,
            version: version.to_string(),
        });
    }

    let jwt_issuer =
        std::env::var("MOZILLA_JWT_ISSUER").context("MOZILLA_JWT_ISSUER env var must be set (AMO API key)")?;
    let jwt_secret =
        std::env::var("MOZILLA_JWT_SECRET").context("MOZILLA_JWT_SECRET env var must be set (AMO API secret)")?;

    let tempdir = tempfile::TempDir::new().context("create staging tempdir")?;
    let staging_dir = tempdir.path();
    extension::stage(staging_dir, version, config).context("stage extension for signing")?;

    // Written BEFORE the upload: a sidecar with no .xpi is never read, and a
    // sign that dies after AMO accepted the version still leaves the record.
    let sidecar = sidecar_path(&artifacts_dir, version);
    std::fs::write(&sidecar, format!("{config_hash}\n")).with_context(|| format!("write {}", sidecar.display()))?;

    log::info!(
        "signing extension v{version} (staged at {}, artifacts -> {})",
        staging_dir.display(),
        artifacts_dir.display()
    );

    // Pass AMO credentials via env (WEB_EXT_API_KEY / WEB_EXT_API_SECRET)
    // rather than `--api-key` / `--api-secret` argv: argv is world-readable in
    // /proc/<pid>/cmdline while the (multi-minute) sign runs, leaking the
    // secrets to any local user.
    let status = Command::new("web-ext")
        .env("WEB_EXT_API_KEY", &jwt_issuer)
        .env("WEB_EXT_API_SECRET", &jwt_secret)
        .args([
            "sign",
            "--channel",
            "unlisted",
            "--source-dir",
            staging_dir.to_str().context("staging dir path is not valid UTF-8")?,
            "--artifacts-dir",
            artifacts_dir
                .to_str()
                .context("artifacts dir path is not valid UTF-8")?,
        ])
        .status()
        .context("Failed to run web-ext - is it installed?")?;

    if !status.success() {
        eyre::bail!(
            "web-ext sign failed with exit {status}; if AMO reported \"already exists\", another host published v{version} first; bump to re-sign"
        );
    }

    let xpi_path = locate_versioned_xpi(&artifacts_dir, version)?;

    Ok(SignResult {
        extension_dir: source_dir,
        xpi_path,
        version: version.to_string(),
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

#[cfg(test)]
mod tests;
