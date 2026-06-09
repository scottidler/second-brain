//! Replay: re-run the ingestion pipeline against existing traces or
//! vault notes. Supports replaying by trace-id, replaying all rejected
//! traces within a time window, and bootstrap-from-vault replay that
//! re-fetches a pre-staging note by reading its frontmatter.

use chrono::{Duration, Utc};
use eyre::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};
use crate::types::{IngestResult, IngestStatus, TraceFilter};

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
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
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
    let response = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("reingest HTTP call to {endpoint}"))?;
    let result: IngestResult = response.json().await.context("parse daemon response")?;
    Ok(result)
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
    if from_stage > 0 {
        bail!(
            "replay: --from-stage {from_stage} not yet supported; only --from-stage 0 \
             (full re-fetch) is wired in this release. Skip the flag to re-run from Stage 0."
        );
    }
    let store = FsArtifactStore::from_config(&config.staging);
    if !store.has_trace(trace_id)? {
        bail!("replay: trace {trace_id} not found in staging");
    }
    let envelope = store.read_envelope(trace_id)?;
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
