//! Replay: re-run the ingestion pipeline against existing traces or
//! vault notes. Supports replaying by trace-id, replaying all rejected
//! traces within a time window, and bootstrap-from-vault replay that
//! re-fetches a pre-staging note by reading its frontmatter.

use chrono::{Duration, Utc};
use eyre::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};
use crate::types::{IngestKind, IngestMethod, IngestResult, IngestStatus, TraceFilter};

#[derive(Debug, Default)]
pub struct ReplayOptions {
    pub trace_id: Option<String>,
    pub from_stage: u8,
    pub since: Option<String>,
    pub rejected: bool,
    pub bootstrap_from_vault: bool,
    pub note: Option<PathBuf>,
    pub dry_run: bool,
}

/// Parse a duration expression ("7d", "24h", "30m") into a Duration.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration");
    }
    // Split off the trailing unit character at a char boundary; the unit is
    // always the last char. `split_at(len - 1)` panicked when the last char
    // was multi-byte (e.g. a malformed "5é"), before the unit check could
    // reject it.
    let split = s.char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
    let (num_part, unit_part) = s.split_at(split);
    let num: i64 = num_part
        .parse()
        .with_context(|| format!("invalid duration number: {s}"))?;
    match unit_part {
        "d" => Ok(Duration::days(num)),
        "h" => Ok(Duration::hours(num)),
        "m" => Ok(Duration::minutes(num)),
        "s" => Ok(Duration::seconds(num)),
        _ => bail!("unknown duration unit in {s}: expected d|h|m|s"),
    }
}

/// Read a vault note's frontmatter and return the `method:` value, if any.
pub fn read_method_from_note(note_path: &Path) -> Result<Option<String>> {
    let text = std::fs::read_to_string(note_path).with_context(|| format!("read note {}", note_path.display()))?;
    let Some(frontmatter) = extract_frontmatter(&text) else {
        return Ok(None);
    };
    for line in frontmatter.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("method:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if !value.is_empty() {
                return Ok(Some(value));
            }
        }
    }
    Ok(None)
}

/// Read a vault note's frontmatter and return the `source:` URL, if any.
pub fn read_source_from_note(note_path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(note_path).with_context(|| format!("read note {}", note_path.display()))?;
    let Some(frontmatter) = extract_frontmatter(&text) else {
        bail!("note {} has no YAML frontmatter", note_path.display());
    };
    for line in frontmatter.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("source:") {
            let value = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            if value.is_empty() {
                bail!("note {} has empty source field", note_path.display());
            }
            return Ok(value);
        }
    }
    bail!("note {} has no source: frontmatter field", note_path.display());
}

fn extract_frontmatter(text: &str) -> Option<&str> {
    vault::frontmatter::split_raw(text).map(|(fm, _body)| fm)
}

/// Outcome of a `borg replay` run. The actual per-trace results stream
/// through the `ReplayEvent` callback (sequential HTTP per trace, per
/// architect Alternative 3); the report holds only the mode summary
/// and aggregate counts sb prints at the end if it wants.
#[derive(Debug)]
pub struct ReplayReport {
    pub mode: ReplayMode,
    pub dry_run: bool,
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

#[derive(Debug)]
pub enum ReplayMode {
    Bootstrap,
    Trace,
    Matching { count: usize },
}

/// Live-progress event emitted by `replay::run`. Variants are typed so
/// no pre-formatted text crosses the lib boundary.
#[derive(Debug)]
pub enum ReplayEvent {
    BootstrapHeader {
        note_path: PathBuf,
        source: String,
        method: String,
    },
    TraceHeader {
        trace_id: String,
        source: String,
    },
    MatchingHeader {
        count: usize,
    },
    NoMatches,
    DryRunBootstrap {
        source: String,
    },
    DryRunTrace,
    ResultOk {
        title: Option<String>,
    },
    ResultDuplicate {
        original_date: String,
    },
    ResultFailed {
        reason: String,
    },
    ResultQueued,
    ResultOther {
        description: String,
    },
    MatchingItemError {
        trace_id: String,
        error: String,
    },
}

/// Dispatch a replay based on the options provided. Streams progress
/// events through `progress`; returns the typed `ReplayReport` once the
/// dispatch finishes (or pass `|_| {}` to ignore events).
pub async fn run(
    config: Config,
    opts: ReplayOptions,
    mut progress: impl FnMut(&ReplayEvent) + Send,
) -> Result<ReplayReport> {
    // Highest priority: bootstrap-from-vault
    if opts.bootstrap_from_vault {
        let Some(note_path) = opts.note.as_ref() else {
            bail!("--bootstrap-from-vault requires --note <path>");
        };
        return bootstrap_note(&config, note_path, opts.dry_run, &mut progress).await;
    }

    // Next: specific trace
    if let Some(trace_id) = &opts.trace_id {
        return replay_trace_top(&config, trace_id, opts.from_stage, opts.dry_run, &mut progress).await;
    }

    // Next: filter over traces in the store
    if opts.rejected || opts.since.is_some() {
        return replay_matching(&config, &opts, &mut progress).await;
    }

    bail!("replay: must provide a trace_id, --since, --rejected, or --bootstrap-from-vault --note");
}

async fn bootstrap_note(
    config: &Config,
    note_path: &Path,
    dry_run: bool,
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
) -> Result<ReplayReport> {
    let source =
        read_source_from_note(note_path).with_context(|| format!("extract source from {}", note_path.display()))?;
    let method = read_method_from_note(note_path)?.unwrap_or_else(|| "cli".to_string());
    progress(&ReplayEvent::BootstrapHeader {
        note_path: note_path.to_path_buf(),
        source: source.clone(),
        method: method.clone(),
    });
    if dry_run {
        progress(&ReplayEvent::DryRunBootstrap { source });
        return Ok(ReplayReport {
            mode: ReplayMode::Bootstrap,
            dry_run: true,
            attempted: 1,
            succeeded: 0,
            failed: 0,
        });
    }
    let result = reingest_via_daemon(config, &source, &method).await?;
    let (succeeded, failed) = emit_result_event(progress, &result.status, &result.title);
    Ok(ReplayReport {
        mode: ReplayMode::Bootstrap,
        dry_run: false,
        attempted: 1,
        succeeded,
        failed,
    })
}

fn emit_result_event(
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
    status: &IngestStatus,
    title: &Option<String>,
) -> (usize, usize) {
    match status {
        IngestStatus::Completed => {
            progress(&ReplayEvent::ResultOk { title: title.clone() });
            (1, 0)
        }
        IngestStatus::Duplicate { original_date } => {
            progress(&ReplayEvent::ResultDuplicate {
                original_date: original_date.clone(),
            });
            (1, 0)
        }
        IngestStatus::Failed { reason } => {
            progress(&ReplayEvent::ResultFailed { reason: reason.clone() });
            (0, 1)
        }
        IngestStatus::Queued => {
            progress(&ReplayEvent::ResultQueued);
            (0, 0)
        }
    }
}

/// Grace added to the pipeline hard timeout before replay gives up polling a
/// trace's terminal state - a trace cannot legitimately stay non-terminal past
/// `hard_timeout + watchdog grace`, so polling beyond that means the daemon
/// crashed; stop rather than hang replay forever.
const POLL_GRACE_SECS: u64 = 90;
/// Interval between `/trace/{id}` polls.
const POLL_INTERVAL_SECS: u64 = 2;

async fn reingest_via_daemon(config: &Config, url: &str, method: &str) -> Result<IngestResult> {
    let host = &config.hotkey.host;
    let port = config.hotkey.port;
    let endpoint = format!("http://{host}:{port}/ingest");
    let body = serde_json::json!({
        "url": url,
        "tags": [],
        "force": true,
        "method": method,
    });
    let client = reqwest::Client::new();
    let mut req = client.post(&endpoint).json(&body);
    if let Some(token) = crate::config::resolve_client_auth_token(&config.server) {
        req = req.bearer_auth(token);
    }
    let response = req
        .send()
        .await
        .with_context(|| format!("reingest HTTP call to {endpoint}"))?;
    let result: IngestResult = response.json().await.context("parse daemon response")?;

    // The daemon dispatches the pipeline in the background and answers
    // `Queued` immediately. Poll `/trace/{id}` for the terminal state so the
    // caller gets real success/failure counts AND so replay paces one entry
    // at a time (the documented sequential pacing - without the poll it
    // silently became enqueue-everything).
    match (&result.status, result.trace_id.as_deref()) {
        (IngestStatus::Queued, Some(trace_id)) => poll_trace_terminal(config, host, port, trace_id).await,
        _ => Ok(result),
    }
}

/// Poll `GET /trace/{trace_id}` until the receipts row reaches a terminal
/// state, or the polling ceiling (`hard_timeout + POLL_GRACE_SECS`) elapses.
/// Shared by replay (`reingest_via_daemon`) and `crate::reingest`.
pub(crate) async fn poll_trace_terminal(
    config: &Config,
    host: &str,
    port: u16,
    trace_id: &str,
) -> Result<IngestResult> {
    let endpoint = format!("http://{host}:{port}/trace/{trace_id}");
    let client = reqwest::Client::new();
    let ceiling = std::time::Duration::from_secs(config.pipeline.hard_timeout_secs + POLL_GRACE_SECS);
    let interval = std::time::Duration::from_secs(POLL_INTERVAL_SECS);
    let auth = crate::config::resolve_client_auth_token(&config.server);
    let start = std::time::Instant::now();
    loop {
        let mut req = client.get(&endpoint);
        if let Some(token) = &auth {
            req = req.bearer_auth(token);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: crate::routes::TraceStateResponse = resp.json().await.context("parse /trace response")?;
                match body.status.as_deref() {
                    Some("succeeded") => {
                        return Ok(IngestResult {
                            status: IngestStatus::Completed,
                            note_path: body.note_path,
                            trace_id: Some(trace_id.to_string()),
                            ..Default::default()
                        });
                    }
                    Some("failed") => {
                        let reason = body.failure_stage.unwrap_or_else(|| "failed".to_string());
                        return Ok(IngestResult {
                            status: IngestStatus::Failed { reason },
                            trace_id: Some(trace_id.to_string()),
                            ..Default::default()
                        });
                    }
                    // Still `received`; keep polling.
                    _ => {}
                }
            }
            Ok(resp) => log::warn!("poll /trace/{trace_id}: HTTP {}", resp.status()),
            Err(e) => log::warn!("poll /trace/{trace_id} failed: {e}"),
        }
        if start.elapsed() > ceiling {
            log::error!("poll /trace/{trace_id}: terminal state not reached within ceiling; daemon may have crashed");
            return Ok(IngestResult {
                status: IngestStatus::Failed {
                    reason: "trace did not reach a terminal state within the polling ceiling".to_string(),
                },
                trace_id: Some(trace_id.to_string()),
                ..Default::default()
            });
        }
        tokio::time::sleep(interval).await;
    }
}

async fn replay_trace_top(
    config: &Config,
    trace_id: &str,
    from_stage: u8,
    dry_run: bool,
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
) -> Result<ReplayReport> {
    let (succeeded, failed) = replay_one(config, trace_id, from_stage, dry_run, progress).await?;
    Ok(ReplayReport {
        mode: ReplayMode::Trace,
        dry_run,
        attempted: 1,
        succeeded,
        failed,
    })
}

/// Replay a single trace and stream events. Returns (succeeded, failed)
/// counts (1/0, 0/1, or 0/0 for dry-run/queued).
async fn replay_one(
    config: &Config,
    trace_id: &str,
    from_stage: u8,
    dry_run: bool,
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
) -> Result<(usize, usize)> {
    if !config.staging.enabled {
        bail!("replay: staging.enabled must be true");
    }
    let store = FsArtifactStore::from_config(&config.staging);
    if !store.has_trace(trace_id)? {
        bail!("replay: trace {trace_id} not found in staging");
    }
    let envelope = store.read_envelope(trace_id)?;

    // Stage-2 replay (re-distill from staged artifacts, NO re-fetch) is wired
    // for the session/harvest kind ONLY (design doc Phase 7): a `clyde://`
    // source cannot be re-POSTed to the daemon the way a URL source can, so it
    // re-derives directly from the staged transcript + member records. Every
    // other kind's `--from-stage > 0` stays explicitly unsupported.
    if from_stage == 2 && matches!(envelope.kind, IngestKind::Session) {
        return replay_session_stage2(config, &store, trace_id, dry_run, progress).await;
    }
    if from_stage > 0 {
        bail!(
            "replay: --from-stage {from_stage} is only supported for session traces \
             (harvest, --from-stage 2); trace {trace_id} is kind={}. Skip the flag to \
             re-run from Stage 0 (full re-fetch).",
            envelope.kind
        );
    }
    let source = String::from_utf8(store.read_body(trace_id)?).context("read body as utf-8")?;
    let source = source.trim().to_string();
    progress(&ReplayEvent::TraceHeader {
        trace_id: trace_id.to_string(),
        source: source.clone(),
    });
    if dry_run {
        progress(&ReplayEvent::DryRunTrace);
        return Ok((0, 0));
    }
    let method = envelope.method.to_string();
    let result = reingest_via_daemon(config, &source, &method).await?;
    Ok(emit_result_event(progress, &result.status, &result.title))
}

/// Stage-2 replay for a harvest session trace: re-derive the note from the
/// staged transcript (`body.txt`) + member records (`members.yml`), without
/// touching clyde. Produces a STRUCTURALLY equivalent note (same
/// `source:`/`trace:`, valid `Distilled`, bounds respected) - byte-identity is
/// not asserted because the distiller is an LLM pass.
///
/// Takes the SAME exclusive harvest state lock the nightly `sb borg harvest`
/// run holds for its whole run (`harvest::run_with`) - a session-trace replay
/// touches the same durable harvest identity a live run owns, so a manual
/// replay and a timer run must not race. A URL replay never calls this
/// function and so never takes this lock (design doc Phase 2: "for SESSION
/// traces only"). `acquire_lock` is `try_lock_exclusive` (never waits), so
/// this fails instantly and loudly with `HarvestLockHeld` naming the lock path
/// rather than blocking or racing.
async fn replay_session_stage2(
    config: &Config,
    store: &FsArtifactStore,
    trace_id: &str,
    dry_run: bool,
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
) -> Result<(usize, usize)> {
    let state_path = vault::paths::borg_harvest_state();
    let _lock = crate::harvest::watermark::acquire_lock(&state_path)?;

    let body = String::from_utf8(store.read_body(trace_id)?).context("read staged body as utf-8")?;
    progress(&ReplayEvent::TraceHeader {
        trace_id: trace_id.to_string(),
        source: format!("clyde session thread ({} transcript bytes)", body.len()),
    });
    if dry_run {
        progress(&ReplayEvent::DryRunTrace);
        return Ok((0, 0));
    }
    let raw = store
        .read_attachment(trace_id, crate::harvest::SESSION_REPLAY_META_FILE)?
        .ok_or_else(|| {
            eyre::eyre!(
                "replay: trace {trace_id} has no {} (staged before Phase 7, or not a harvest \
                 session note); cannot re-derive from stage 2",
                crate::harvest::SESSION_REPLAY_META_FILE
            )
        })?;
    let meta: crate::harvest::SessionReplayMeta =
        serde_yaml::from_slice(&raw).context("parse staged session replay metadata (members.yml)")?;

    // `ResolveIntent::Replay` is what lands this re-derivation on the note the
    // trace already produced: the publish path resolves the prior note by
    // identity (trace + source + body hash) and writes THAT path. `force=true`
    // is retained for the miss case only - a trace whose note was deleted
    // republishes as a new note under the bare `{slug}.md` rather than a
    // `--<id8>` sibling. It never meant "overwrite in place" (the filename stem
    // is model output, so the bare slug is a file that has never existed); that
    // misreading is the bug this design fixes.
    let result = crate::pipeline::session::process_session(
        &body,
        &meta.members,
        &meta.primary_id,
        meta.body_truncated,
        Vec::new(),
        IngestMethod::Harvest,
        true,
        crate::harvest::identity::ResolveIntent::Replay,
        config,
        trace_id,
    )
    .await;
    Ok(emit_result_event(progress, &result.status, &result.title))
}

async fn replay_matching(
    config: &Config,
    opts: &ReplayOptions,
    progress: &mut (impl FnMut(&ReplayEvent) + ?Sized),
) -> Result<ReplayReport> {
    if !config.staging.enabled {
        bail!("replay: staging.enabled must be true");
    }
    let store = FsArtifactStore::from_config(&config.staging);
    let since = opts
        .since
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .map(|d| (Utc::now() - d).to_rfc3339());
    let filter = TraceFilter {
        rejected_only: opts.rejected,
        since,
        ..TraceFilter::default()
    };
    let matches = store.list_traces(&filter)?;
    if matches.is_empty() {
        progress(&ReplayEvent::NoMatches);
        return Ok(ReplayReport {
            mode: ReplayMode::Matching { count: 0 },
            dry_run: opts.dry_run,
            attempted: 0,
            succeeded: 0,
            failed: 0,
        });
    }
    progress(&ReplayEvent::MatchingHeader { count: matches.len() });
    let mut attempted = 0usize;
    let mut succeeded_total = 0usize;
    let mut failed_total = 0usize;
    for trace_id in &matches {
        attempted += 1;
        match replay_one(config, trace_id, opts.from_stage, opts.dry_run, progress).await {
            Ok((s, f)) => {
                succeeded_total += s;
                failed_total += f;
            }
            Err(e) => {
                progress(&ReplayEvent::MatchingItemError {
                    trace_id: trace_id.clone(),
                    error: format!("{e:#}"),
                });
                failed_total += 1;
            }
        }
    }
    Ok(ReplayReport {
        mode: ReplayMode::Matching { count: matches.len() },
        dry_run: opts.dry_run,
        attempted,
        succeeded: succeeded_total,
        failed: failed_total,
    })
}

#[cfg(test)]
mod tests;
