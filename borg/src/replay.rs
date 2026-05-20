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
    let (num_part, unit_part) = s.split_at(s.len().saturating_sub(1));
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

/// Dispatch a replay based on the options provided. Returns the lines sb
/// should print so the lib stays stdout-clean.
pub async fn run(config: Config, opts: ReplayOptions) -> Result<Vec<String>> {
    // Highest priority: bootstrap-from-vault
    if opts.bootstrap_from_vault {
        let Some(note_path) = opts.note.as_ref() else {
            bail!("--bootstrap-from-vault requires --note <path>");
        };
        return bootstrap_note(&config, note_path, opts.dry_run).await;
    }

    // Next: specific trace
    if let Some(trace_id) = &opts.trace_id {
        return replay_trace(&config, trace_id, opts.from_stage, opts.dry_run).await;
    }

    // Next: filter over traces in the store
    if opts.rejected || opts.since.is_some() {
        return replay_matching(&config, &opts).await;
    }

    bail!("replay: must provide a trace_id, --since, --rejected, or --bootstrap-from-vault --note");
}

async fn bootstrap_note(config: &Config, note_path: &Path, dry_run: bool) -> Result<Vec<String>> {
    let source =
        read_source_from_note(note_path).with_context(|| format!("extract source from {}", note_path.display()))?;
    let method = read_method_from_note(note_path)?.unwrap_or_else(|| "cli".to_string());
    let mut lines = vec![format!(
        "bootstrap: {} -> {} (method: {method})",
        note_path.display(),
        source
    )];
    if dry_run {
        lines.push(format!("  [dry-run] would re-ingest {source}"));
        return Ok(lines);
    }
    let result = reingest_via_daemon(config, &source, &method).await?;
    lines.push(match &result.status {
        IngestStatus::Completed => format!("  -> {}", result.title.as_deref().unwrap_or("(no title)")),
        IngestStatus::Duplicate { original_date } => format!("  -> duplicate (originally ingested {original_date})"),
        IngestStatus::Failed { reason } => format!("  -> failed: {reason}"),
        IngestStatus::Queued => "  -> queued".to_string(),
    });
    Ok(lines)
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

async fn replay_trace(config: &Config, trace_id: &str, from_stage: u8, dry_run: bool) -> Result<Vec<String>> {
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
    let mut lines = vec![format!("replay trace {trace_id}: {source}")];
    if dry_run {
        lines.push("  [dry-run] would re-ingest via daemon".to_string());
        return Ok(lines);
    }
    let method = envelope.method.to_string();
    let result = reingest_via_daemon(config, &source, &method).await?;
    lines.push(match &result.status {
        IngestStatus::Completed => format!("  -> {}", result.title.as_deref().unwrap_or("(no title)")),
        IngestStatus::Failed { reason } => format!("  -> failed: {reason}"),
        other => format!("  -> {other:?}"),
    });
    Ok(lines)
}

async fn replay_matching(config: &Config, opts: &ReplayOptions) -> Result<Vec<String>> {
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
        return Ok(vec!["replay: no traces matched".to_string()]);
    }
    let mut lines = vec![format!("replay: {} matching trace(s)", matches.len())];
    for trace_id in matches {
        match replay_trace(config, &trace_id, opts.from_stage, opts.dry_run).await {
            Ok(trace_lines) => lines.extend(trace_lines),
            Err(e) => lines.push(format!("  trace {trace_id}: {e:#}")),
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests;
