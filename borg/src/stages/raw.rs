use chrono::Utc;
use eyre::{Context, Result, bail};
use std::collections::HashMap;

use crate::blocklist::{self, Blocklist};
use crate::config::Config;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore, sha256_hex};
use crate::stages::classify as gate1;
use crate::types::{ContentKind, Envelope, FetchMeta, GateId, IngestKind, IngestMethod, RejectionRecord, StageKind};

/// Classify the primary `IngestKind` for a capture. Non-destructive: the Stage-0
/// write persists the full raw event regardless of what this returns.
pub fn classify(content: &ContentKind) -> IngestKind {
    match content {
        ContentKind::Text(body) => classify_text(body.trim()),
        ContentKind::Url(url) => classify_url(url),
        ContentKind::Image { .. } | ContentKind::Pdf { .. } | ContentKind::Document { .. } => IngestKind::Image,
        ContentKind::Audio { .. } => IngestKind::VoiceNote,
    }
}

fn classify_text(trimmed: &str) -> IngestKind {
    if trimmed.starts_with("vocab:en ") || trimmed.starts_with("vocab:en:") {
        return IngestKind::VocabularyEn;
    }
    if trimmed.starts_with("vocab:es ") || trimmed.starts_with("vocab:es:") {
        return IngestKind::VocabularyEs;
    }
    if trimmed.starts_with("idea:") {
        return IngestKind::Idea;
    }
    if let Some(first_url) = extract_first_url(trimmed) {
        return classify_url(&first_url);
    }
    IngestKind::Idea
}

fn classify_url(url: &str) -> IngestKind {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default();
    if host.ends_with("github.com") {
        return IngestKind::GitHubUrl;
    }
    if host.ends_with("youtube.com") || host == "youtu.be" || host.ends_with(".youtube.com") {
        return IngestKind::YoutubeUrl;
    }
    if host == "x.com"
        || host.ends_with(".x.com")
        || host == "twitter.com"
        || host.ends_with(".twitter.com")
        || host.ends_with("reddit.com")
        || host.ends_with("news.ycombinator.com")
    {
        return IngestKind::ThreadUrl;
    }
    IngestKind::ArticleUrl
}

/// Extract the first http(s) URL in a text body, if any.
pub fn extract_first_url(body: &str) -> Option<String> {
    for token in body.split_whitespace() {
        if token.starts_with("http://") || token.starts_with("https://") {
            return Some(
                token
                    .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '/')
                    .to_string(),
            );
        }
    }
    None
}

/// Persist the Stage-0 envelope + body + attachments for a capture event.
/// Does not perform network I/O. URL captures still need a subsequent call to
/// `persist_fetched` once the fetcher chain runs.
pub fn write_capture<S: ArtifactStore>(
    store: &S,
    trace_id: &str,
    content: &ContentKind,
    method: IngestMethod,
    origin_message_id: Option<String>,
    extra: HashMap<String, serde_yaml::Value>,
) -> Result<Envelope> {
    let kind = classify(content);
    let envelope = Envelope {
        trace: trace_id.to_string(),
        kind,
        method,
        received_at: Utc::now().to_rfc3339(),
        origin_message_id,
        extra,
    };
    store.write_envelope(trace_id, &envelope)?;
    match content {
        ContentKind::Text(body) => {
            store.write_body(trace_id, body.as_bytes())?;
        }
        ContentKind::Url(url) => {
            store.write_body(trace_id, url.as_bytes())?;
        }
        ContentKind::Image { data, filename } => {
            store.write_attachment(trace_id, filename, data)?;
        }
        ContentKind::Pdf { data, filename } => {
            store.write_attachment(trace_id, filename, data)?;
        }
        ContentKind::Document { data, filename } => {
            store.write_attachment(trace_id, filename, data)?;
        }
        ContentKind::Audio { data, filename } => {
            store.write_attachment(trace_id, filename, data)?;
        }
    }
    Ok(envelope)
}

/// Stage-0 entry hook called at the top of `pipeline::process_content`.
///
/// When `staging.enabled` is true: writes envelope/body/attachments to the
/// configured artifact store and runs Gate-0 (domain blocklist) for URL
/// captures; on rejection, returns an error the caller converts into a
/// Failed ingest result. When staging is disabled this is a no-op.
pub fn stage_0_init(config: &Config, content: &ContentKind, method: IngestMethod, trace_id: &str) -> Result<()> {
    if !config.staging.enabled {
        return Ok(());
    }
    let store = FsArtifactStore::from_config(&config.staging);

    if let ContentKind::Url(url) = content {
        let blocklist_path = blocklist::default_path();
        let blocklist = Blocklist::from_file(&blocklist_path).unwrap_or_else(|e| {
            log::warn!("stage_0_init: blocklist load failed, treating as empty: {e:#}");
            Blocklist::default()
        });
        let now = Utc::now();
        if let Err(err) = blocklist::gate_0(&blocklist, url, now, ()) {
            log::warn!("[{trace_id}] Gate-0 reject: {err:#}");
            bail!("{err}");
        }
    }
    write_capture(&store, trace_id, content, method, None, HashMap::new())
        .with_context(|| format!("stage_0_init: write_capture for trace {trace_id}"))?;
    Ok(())
}

/// Persist the bytes of a successful URL fetch to the artifact store. Called
/// from the existing URL processing paths (`process_article_fabric` /
/// `process_article_jina`) immediately after the fetch succeeds, so Stage 1
/// in Phase 3 can read the bytes offline and Gate-1 can pattern-match block
/// pages on the raw response. No-op when staging is disabled.
pub fn persist_fetched_if_staging(
    config: &Config,
    trace_id: &str,
    url: &str,
    bytes: &[u8],
    extractor: &str,
    status: u16,
    content_type: Option<&str>,
) -> Result<()> {
    if !config.staging.enabled {
        return Ok(());
    }
    let store = FsArtifactStore::from_config(&config.staging);
    let meta = FetchMeta {
        source: url.to_string(),
        extractor: extractor.to_string(),
        status,
        content_type: content_type.map(String::from),
        bytes: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        fallbacks_attempted: Vec::new(),
    };
    store.write_fetched(trace_id, bytes, &meta)?;
    Ok(())
}

/// Gate-1: block-page detection on the raw Stage-0 fetched bytes. When a match
/// fires, this function:
///
/// - Adds the URL's registrable domain to the blocklist (with the parsed
///   `retriable-after` timestamp).
/// - Persists the blocklist to disk.
/// - Writes a rejection record to the artifact store.
/// - Returns `Err` so the caller converts the ingestion to Failed.
///
/// No-op when staging is disabled.
pub fn run_gate_1(config: &Config, trace_id: &str, url: &str, bytes: &[u8], status: u16) -> Result<()> {
    if !config.staging.enabled {
        return Ok(());
    }
    let now = Utc::now();
    let Some(matched) = gate1::detect_block_page(bytes, status, now) else {
        return Ok(());
    };
    let domain = blocklist::domain_for(url);
    log::warn!(
        "[{trace_id}] Gate-1 reject: domain {domain} {reason} retriable-after={retry}",
        reason = matched.reason,
        retry = matched.retriable_after.to_rfc3339(),
    );
    let blocklist_path = blocklist::default_path();
    let mut bl = Blocklist::from_file(&blocklist_path).unwrap_or_default();
    bl.add_or_refresh(&domain, &matched.reason, matched.retriable_after);
    if let Err(e) = bl.save_to(&blocklist_path) {
        log::warn!("[{trace_id}] Gate-1: blocklist save failed: {e:#}");
    }
    let store = FsArtifactStore::from_config(&config.staging);
    let rec = RejectionRecord {
        trace: trace_id.to_string(),
        stage: StageKind::Transcript,
        gate: GateId::BlockPage,
        reason: matched.reason.clone(),
        rejected_at: now.to_rfc3339(),
        raw_artifact: Some(format!("{trace_id}/fetched.html")),
        source: Some(url.to_string()),
        domain: Some(domain.clone()),
        blocklist_updated: true,
        retriable_after: Some(matched.retriable_after.to_rfc3339()),
    };
    if let Err(e) = store.write_rejection(trace_id, &rec) {
        log::warn!("[{trace_id}] Gate-1: rejection write failed: {e:#}");
    }
    bail!("gate-1: {}", matched.reason);
}

#[cfg(test)]
mod tests;
