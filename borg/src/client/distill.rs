//! `sb borg distill <URL> [--output DIR]`: run the pipeline on the daemon with
//! `PublishPolicy::Scratch` and download what it left (design:
//! docs/design/2026-10-25-borg-distill.md).
//!
//! Order: refuse an explicit non-empty `DIR`, check `/health` for the
//! `distill` feature, `POST /distill`, poll `/trace/{id}`, fetch the manifest,
//! check every manifest path, then download each file into `DIR`. Every error
//! after the POST names the trace id, because the files stay on the daemon for
//! the retention window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eyre::{Context, Result};
use vault::daemon::client::DaemonClient;

use crate::config::Config;
use crate::health::{FEATURE_DISTILL, HealthResponse};
use crate::replay::{self, PollOutcome};
use crate::scratch::{DistillManifest, DistillRequest, requested_path};
use crate::types::{IngestResult, IngestStatus};

/// What a finished `sb borg distill` left behind.
#[derive(Debug)]
pub struct DistillRun {
    /// The directory the files were written to.
    pub dir: PathBuf,
    pub trace_id: String,
    /// The rendered note, `dir`-joined, when the manifest listed one at the top level.
    pub note: Option<PathBuf>,
    /// `Some(reason)` when the trace ended `failed`; its partial files were still written.
    pub failure: Option<String>,
}

/// Run a distill. `output` is the explicit `--output`; `None` means
/// `./<trace-id>/`. An `Err` is exit 1; `Ok` with `failure` set is exit 3.
pub async fn distill(config: &Config, url: &str, output: Option<PathBuf>) -> Result<DistillRun> {
    distill_within(
        config,
        url,
        output,
        replay::polling_ceiling(config),
        replay::poll_interval(),
    )
    .await
}

/// [`distill`] with the poll ceiling and interval named, so a test can bound
/// the wait without sleeping for the pipeline's hard timeout.
pub(crate) async fn distill_within(
    config: &Config,
    url: &str,
    output: Option<PathBuf>,
    ceiling: Duration,
    interval: Duration,
) -> Result<DistillRun> {
    log::debug!("distill: url={url} output={output:?}");
    if let Some(dir) = &output {
        refuse_non_empty(dir)?;
    }
    let daemon = DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref())?;
    require_distill_feature(&daemon).await?;

    let body = serde_json::to_value(DistillRequest { url: url.to_string() })?;
    let resp = daemon
        .post_json("/distill", &body)
        .await
        .map_err(|e| connect_hint(e, config))?;
    let queued: IngestResult = daemon.json("/distill", resp).await?;
    if let IngestStatus::Failed { reason } = &queued.status {
        eyre::bail!(
            "the daemon refused the distill{}: {reason}",
            queued.trace_id.map(|t| format!(" (trace {t})")).unwrap_or_default()
        );
    }
    let trace_id = queued
        .trace_id
        .ok_or_else(|| eyre::eyre!("the daemon answered POST /distill with no trace id"))?;
    log::info!("distill: queued trace={trace_id}");

    finish(config, &daemon, &trace_id, output, ceiling, interval)
        .await
        .wrap_err_with(|| format!("distill trace {trace_id} did not finish; its files stay on the daemon"))
}

async fn finish(
    config: &Config,
    daemon: &DaemonClient,
    trace_id: &str,
    output: Option<PathBuf>,
    ceiling: Duration,
    interval: Duration,
) -> Result<DistillRun> {
    let dir = match output {
        Some(dir) => dir,
        None => {
            let dir = PathBuf::from(".").join(trace_id);
            refuse_non_empty(&dir)?;
            dir
        }
    };

    let failure = match replay::poll_trace_terminal_within(daemon, trace_id, ceiling, interval).await? {
        PollOutcome::Terminal(IngestResult {
            status: IngestStatus::Failed { reason },
            ..
        }) => Some(reason),
        PollOutcome::Terminal(_) => None,
        PollOutcome::CeilingReached => eyre::bail!(
            "the trace was not terminal after {}; it may still be queued or running",
            humantime::format_duration(ceiling)
        ),
    };

    let manifest_path = format!("/distill/{trace_id}");
    let resp = daemon.get(&manifest_path, &[], None).await?;
    let manifest: DistillManifest = daemon.json(&manifest_path, resp).await?;

    // Every path is checked before the first byte is written: a buggy daemon
    // cannot make the client write outside `dir`, or half-write a bad listing.
    let planned = manifest
        .files
        .iter()
        .map(|file| Ok((checked_destination(&dir, &file.path)?, file)))
        .collect::<Result<Vec<_>>>()?;

    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (dest, file) in planned {
        let url_path = format!("/distill/{trace_id}/file/{}", encode_path(&file.path));
        let resp = daemon.get(&url_path, &[], Some(config.hotkey.download_timeout)).await?;
        let resp = daemon.ensure_success(&url_path, resp).await?;
        let bytes = resp.bytes().await.with_context(|| format!("download {}", file.path))?;
        if bytes.len() as u64 != file.size {
            eyre::bail!(
                "{} arrived as {} bytes, the manifest said {}",
                file.path,
                bytes.len(),
                file.size
            );
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        write_new(&dest, &bytes)?;
    }

    let note = manifest
        .files
        .iter()
        .find(|f| !f.path.contains('/') && f.path.ends_with(".md"))
        .map(|f| dir.join(&f.path));
    Ok(DistillRun {
        dir,
        trace_id: trace_id.to_string(),
        note,
        failure,
    })
}

/// Refuse a `dir` that exists and holds anything: distill never merges into or
/// overwrites a directory.
fn refuse_non_empty(dir: &Path) -> Result<()> {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                eyre::bail!("{} is not empty; distill never merges into a directory", dir.display());
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) if dir.is_file() => eyre::bail!("{} exists and is a file, not a directory", dir.display()),
        Err(e) => Err(e).with_context(|| format!("read {}", dir.display())),
    }
}

/// Write `bytes` to a file that must not exist yet. The non-empty check runs
/// minutes before the downloads, so a second distill into the same `DIR` (or
/// a duplicate manifest entry) would otherwise merge or overwrite; here it is
/// a download error instead.
fn write_new(dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .with_context(|| format!("create {} (distill never overwrites a file)", dest.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", dest.display()))
}

/// Refuse a daemon that does not advertise `distill`: an older one would 404
/// the route, or (worse, were the route a field) publish into the vault.
async fn require_distill_feature(daemon: &DaemonClient) -> Result<()> {
    let resp = daemon
        .get("/health", &[], None)
        .await
        .with_context(|| format!("checking the daemon at {} before distilling", daemon.addr()))?;
    let health: HealthResponse = daemon.json("/health", resp).await?;
    log::debug!(
        "distill::require_distill_feature: version={} features={:?}",
        health.version,
        health.features
    );
    if !health.supports(FEATURE_DISTILL) {
        eyre::bail!(
            "the daemon at {} (version {}) has no `{FEATURE_DISTILL}` feature; deploy it (`otto deploy`) before `sb borg distill`",
            daemon.addr(),
            health.version
        );
    }
    Ok(())
}

fn connect_hint(e: vault::daemon::client::DaemonError, config: &Config) -> eyre::Report {
    if e.is_connect() {
        eyre::eyre!(
            "cannot reach the borg daemon at http://{}:{} - is the daemon running?",
            config.hotkey.host,
            config.hotkey.port
        )
    } else {
        e.into()
    }
}

/// The path a manifest entry is written to: untrusted input, so an absolute or
/// `..` path is refused, and the joined path must still sit under `dir`.
fn checked_destination(dir: &Path, manifest_path: &str) -> Result<PathBuf> {
    let clean = requested_path(manifest_path).map_err(|why| eyre::eyre!("manifest path {manifest_path:?}: {why}"))?;
    if clean.as_os_str().is_empty() {
        eyre::bail!("manifest path {manifest_path:?} names no file");
    }
    let dest = dir.join(&clean);
    if !dest.starts_with(dir) {
        eyre::bail!("manifest path {manifest_path:?} resolves outside {}", dir.display());
    }
    Ok(dest)
}

/// Percent-encode each `/`-separated segment, so a file name with `?`, `#` or
/// a space still reaches the file route as one path.
fn encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests;
