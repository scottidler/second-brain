//! Export reader: the ONE coupling surface between harvest and clyde. Shells
//! out to the `clyde` binary and parses its versioned JSON contract
//! ([`super::contract`]). Everything downstream of this module is
//! source-agnostic - if clyde is ever abandoned, only this reader is rewritten
//! (design doc Risks: "coupling surface is ONE reader module against a JSON
//! shape - the yt-dlp pattern").
//!
//! The reader is a trait ([`ExportReader`]) so selection/clustering/watermark
//! logic can be exercised against in-memory fixtures without the binary. The
//! production impl ([`ClydeExportReader`]) mirrors borg's established
//! subprocess hygiene (`youtube.rs`): `kill_on_drop(true)` + a wall-clock
//! timeout + concurrent stdout/stderr drain via `wait_with_output`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Context, Result, bail};
use tokio::process::Command as TokioCommand;

use super::contract::{SessionExport, SessionRecord, parse_export};

/// Wall-clock cap on any single `clyde session export` invocation. A bulk
/// metadata page is fast; a `--with-body` read of a very long transcript is
/// the worst case and is still comfortably bounded.
pub const DEFAULT_CLYDE_TIMEOUT_SECS: u64 = 120;

/// The clyde-export port. Async because the only impl shells out; the pure
/// selection/clustering logic never touches it.
#[allow(async_fn_in_trait)]
pub trait ExportReader {
    /// A bulk-metadata page. Steady state passes `cursor` (opaque revision);
    /// a fresh install with no watermark passes `since` (human-time span on
    /// `modified`). clyde ANDs them if both are set; harvest sends one.
    async fn export_bulk(&self, cursor: Option<i64>, since: Option<&str>) -> Result<SessionExport>;

    /// One session's full record WITH its parsed transcript body. The identity
    /// anchor for re-appearance: harvest hashes the returned body.
    async fn export_with_body(&self, id: &str) -> Result<SessionRecord>;
}

/// Production reader: shells out to the configured clyde binary.
#[derive(Debug, Clone)]
pub struct ClydeExportReader {
    binary: PathBuf,
    timeout_secs: u64,
}

impl ClydeExportReader {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout_secs: DEFAULT_CLYDE_TIMEOUT_SECS,
        }
    }

    pub fn with_timeout(binary: impl Into<PathBuf>, timeout_secs: u64) -> Self {
        Self {
            binary: binary.into(),
            timeout_secs,
        }
    }

    /// Run `clyde session export <args>` and return its stdout bytes. Loud on
    /// spawn failure (binary missing), timeout, and non-zero exit.
    async fn run(&self, args: &[String]) -> Result<Vec<u8>> {
        log::debug!(
            "harvest::ClydeExportReader::run: binary={} args={:?} timeout={}s",
            self.binary.display(),
            args,
            self.timeout_secs
        );
        let child = TokioCommand::new(&self.binary)
            .arg("session")
            .arg("export")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn clyde at {} - is it installed?", self.binary.display()))?;

        let output = match tokio::time::timeout(Duration::from_secs(self.timeout_secs), child.wait_with_output()).await
        {
            Ok(res) => res.context("failed to wait for clyde session export")?,
            Err(_) => bail!(
                "clyde session export timed out after {}s (args {:?})",
                self.timeout_secs,
                args
            ),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "clyde session export exited {} (args {:?}): {}",
                output.status,
                args,
                stderr.trim()
            );
        }
        log::debug!(
            "harvest::ClydeExportReader::run: ok stdout_bytes={} args={:?}",
            output.stdout.len(),
            args
        );
        Ok(output.stdout)
    }
}

impl ExportReader for ClydeExportReader {
    async fn export_bulk(&self, cursor: Option<i64>, since: Option<&str>) -> Result<SessionExport> {
        log::debug!(
            "harvest::ClydeExportReader::export_bulk: cursor={:?} since={:?}",
            cursor,
            since
        );
        let mut args = Vec::new();
        if let Some(c) = cursor {
            args.push("--cursor".to_string());
            args.push(c.to_string());
        }
        if let Some(s) = since {
            args.push("--since".to_string());
            args.push(s.to_string());
        }
        let bytes = self.run(&args).await?;
        parse_export(&bytes)
    }

    async fn export_with_body(&self, id: &str) -> Result<SessionRecord> {
        log::debug!("harvest::ClydeExportReader::export_with_body: id={id}");
        let args = vec!["--id".to_string(), id.to_string(), "--with-body".to_string()];
        let bytes = self.run(&args).await?;
        let mut export = parse_export(&bytes)?;
        if export.sessions.is_empty() {
            bail!("clyde session export --id {id} --with-body returned no session");
        }
        Ok(export.sessions.remove(0))
    }
}

/// Convenience for callers holding a config path (Phase 5/6 wiring).
pub fn reader_for(binary: &Path) -> ClydeExportReader {
    ClydeExportReader::new(binary)
}
