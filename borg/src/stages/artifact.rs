use crate::config::{StagingConfig, StagingLayout};
use crate::types::{
    Envelope, FetchMeta, IngestKind, IngestMethod, RawCapture, RejectionRecord, TraceFilter, TraceMeta,
};
use chrono::{DateTime, Utc};
use eyre::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Filesystem layout of a single trace. Stage 0 writes envelope + body +
/// attachments (+ fetched.* when a URL is present); Stage 1 writes transcript.*;
/// Stage 2 writes summary.*; any gate writes rejection.yml.
pub trait ArtifactStore: Send + Sync {
    /// Write the envelope sidecar. Creates the trace directory as a side effect.
    fn write_envelope(&self, trace_id: &str, envelope: &Envelope) -> Result<()>;

    /// Read the envelope sidecar.
    fn read_envelope(&self, trace_id: &str) -> Result<Envelope>;

    /// Write the message body (utf-8 or binary). May be empty.
    fn write_body(&self, trace_id: &str, bytes: &[u8]) -> Result<()>;

    /// Read the message body bytes.
    fn read_body(&self, trace_id: &str) -> Result<Vec<u8>>;

    /// Write a binary attachment (image, audio, pdf, ...).
    fn write_attachment(&self, trace_id: &str, filename: &str, bytes: &[u8]) -> Result<()>;

    /// Write fetched URL response bytes + fetch metadata. Atomic (temp-then-rename).
    fn write_fetched(&self, trace_id: &str, bytes: &[u8], meta: &FetchMeta) -> Result<()>;

    /// Read fetched URL bytes if present.
    fn read_fetched(&self, trace_id: &str) -> Result<Option<(Vec<u8>, FetchMeta)>>;

    /// Assemble the full raw view Stage 1 consumes. Never reaches the network.
    fn read_raw(&self, trace_id: &str) -> Result<RawCapture>;

    /// Write a transcript with its metadata sidecar.
    fn write_transcript(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()>;

    /// Read a transcript + metadata sidecar.
    fn read_transcript(&self, trace_id: &str) -> Result<(String, TraceMeta)>;

    /// Write a summary with its metadata sidecar.
    fn write_summary(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()>;

    /// Read a summary + metadata sidecar.
    fn read_summary(&self, trace_id: &str) -> Result<(String, TraceMeta)>;

    /// Write a gate-rejection record. Presence flags the trace as rejected.
    fn write_rejection(&self, trace_id: &str, rec: &RejectionRecord) -> Result<()>;

    /// Read a gate-rejection record if one exists.
    fn read_rejection(&self, trace_id: &str) -> Result<Option<RejectionRecord>>;

    /// List trace IDs matching a filter. Order is unspecified.
    fn list_traces(&self, filter: &TraceFilter) -> Result<Vec<String>>;

    /// Check whether a trace directory exists in the store.
    fn has_trace(&self, trace_id: &str) -> Result<bool>;

    /// Delete every artifact for the given trace. No-op if absent.
    fn delete_trace(&self, trace_id: &str) -> Result<()>;
}

/// Filesystem-backed `ArtifactStore`. Per-trace layout is the default; per-stage
/// layout is a config-flag alternative (see `StagingLayout`).
#[derive(Debug, Clone)]
pub struct FsArtifactStore {
    root: PathBuf,
    layout: StagingLayout,
}

impl FsArtifactStore {
    pub fn new(root: impl Into<PathBuf>, layout: StagingLayout) -> Self {
        Self {
            root: root.into(),
            layout,
        }
    }

    pub fn from_config(config: &StagingConfig) -> Self {
        Self::new(config.root.clone(), config.layout)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn layout(&self) -> StagingLayout {
        self.layout
    }

    fn trace_dir(&self, trace_id: &str) -> PathBuf {
        match self.layout {
            StagingLayout::PerTrace => self.root.join(trace_id),
            StagingLayout::PerStage => self.root.join("raw").join(trace_id),
        }
    }

    fn stage_file(&self, trace_id: &str, filename: &str, stage_dir: &str) -> PathBuf {
        match self.layout {
            StagingLayout::PerTrace => self.trace_dir(trace_id).join(filename),
            StagingLayout::PerStage => {
                let ext = filename
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_string())
                    .unwrap_or_else(|| "md".to_string());
                self.root.join(stage_dir).join(format!("{trace_id}.{ext}"))
            }
        }
    }

    fn envelope_path(&self, trace_id: &str) -> PathBuf {
        self.trace_dir(trace_id).join("envelope.yml")
    }

    fn body_path(&self, trace_id: &str) -> PathBuf {
        self.trace_dir(trace_id).join("body.txt")
    }

    fn attachment_path(&self, trace_id: &str, filename: &str) -> PathBuf {
        self.trace_dir(trace_id).join("attachments").join(filename)
    }

    fn fetched_bytes_path(&self, trace_id: &str) -> PathBuf {
        self.trace_dir(trace_id).join("fetched.html")
    }

    fn fetched_meta_path(&self, trace_id: &str) -> PathBuf {
        self.trace_dir(trace_id).join("fetched.yml")
    }

    fn transcript_md_path(&self, trace_id: &str) -> PathBuf {
        self.stage_file(trace_id, "transcript.md", "transcripts")
    }

    fn transcript_meta_path(&self, trace_id: &str) -> PathBuf {
        self.stage_file(trace_id, "transcript.yml", "transcripts")
    }

    fn summary_md_path(&self, trace_id: &str) -> PathBuf {
        self.stage_file(trace_id, "summary.md", "summaries")
    }

    fn summary_meta_path(&self, trace_id: &str) -> PathBuf {
        self.stage_file(trace_id, "summary.yml", "summaries")
    }

    fn rejection_path(&self, trace_id: &str) -> PathBuf {
        self.stage_file(trace_id, "rejection.yml", "rejections")
    }

    fn trace_roots(&self) -> Vec<PathBuf> {
        match self.layout {
            StagingLayout::PerTrace => vec![self.root.clone()],
            StagingLayout::PerStage => vec![self.root.join("raw")],
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create parent dir for {}", path.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

impl ArtifactStore for FsArtifactStore {
    fn write_envelope(&self, trace_id: &str, envelope: &Envelope) -> Result<()> {
        let bytes = serde_yaml::to_string(envelope).context("serialize envelope")?;
        atomic_write(&self.envelope_path(trace_id), bytes.as_bytes())
    }

    fn read_envelope(&self, trace_id: &str) -> Result<Envelope> {
        let text = std::fs::read_to_string(self.envelope_path(trace_id))
            .with_context(|| format!("read envelope for trace {trace_id}"))?;
        serde_yaml::from_str(&text).context("parse envelope yaml")
    }

    fn write_body(&self, trace_id: &str, bytes: &[u8]) -> Result<()> {
        atomic_write(&self.body_path(trace_id), bytes)
    }

    fn read_body(&self, trace_id: &str) -> Result<Vec<u8>> {
        std::fs::read(self.body_path(trace_id)).with_context(|| format!("read body for trace {trace_id}"))
    }

    fn write_attachment(&self, trace_id: &str, filename: &str, bytes: &[u8]) -> Result<()> {
        atomic_write(&self.attachment_path(trace_id, filename), bytes)
    }

    fn write_fetched(&self, trace_id: &str, bytes: &[u8], meta: &FetchMeta) -> Result<()> {
        atomic_write(&self.fetched_bytes_path(trace_id), bytes)?;
        let yml = serde_yaml::to_string(meta).context("serialize fetch meta")?;
        atomic_write(&self.fetched_meta_path(trace_id), yml.as_bytes())
    }

    fn read_fetched(&self, trace_id: &str) -> Result<Option<(Vec<u8>, FetchMeta)>> {
        let bytes_path = self.fetched_bytes_path(trace_id);
        let meta_path = self.fetched_meta_path(trace_id);
        if !bytes_path.exists() || !meta_path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&bytes_path).with_context(|| format!("read {}", bytes_path.display()))?;
        let meta_text = std::fs::read_to_string(&meta_path).with_context(|| format!("read {}", meta_path.display()))?;
        let meta: FetchMeta = serde_yaml::from_str(&meta_text).context("parse fetch meta yaml")?;
        Ok(Some((bytes, meta)))
    }

    fn read_raw(&self, trace_id: &str) -> Result<RawCapture> {
        let envelope = self.read_envelope(trace_id)?;
        let body = self.read_body(trace_id).unwrap_or_default();
        let mut attachments = HashMap::new();
        let att_dir = self.trace_dir(trace_id).join("attachments");
        if att_dir.is_dir() {
            for entry in std::fs::read_dir(&att_dir).with_context(|| format!("read_dir {}", att_dir.display()))? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let filename = path.file_name().and_then(|f| f.to_str()).map(|s| s.to_string());
                if let Some(name) = filename {
                    let bytes = std::fs::read(&path)?;
                    attachments.insert(name, bytes);
                }
            }
        }
        let fetched = self.read_fetched(trace_id)?;
        Ok(RawCapture {
            envelope,
            body,
            attachments,
            fetched,
        })
    }

    fn write_transcript(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()> {
        atomic_write(&self.transcript_md_path(trace_id), text.as_bytes())?;
        let yml = serde_yaml::to_string(meta).context("serialize transcript meta")?;
        atomic_write(&self.transcript_meta_path(trace_id), yml.as_bytes())
    }

    fn read_transcript(&self, trace_id: &str) -> Result<(String, TraceMeta)> {
        let text_path = self.transcript_md_path(trace_id);
        let meta_path = self.transcript_meta_path(trace_id);
        let text = std::fs::read_to_string(&text_path).with_context(|| format!("read {}", text_path.display()))?;
        let meta_text = std::fs::read_to_string(&meta_path).with_context(|| format!("read {}", meta_path.display()))?;
        let meta: TraceMeta = serde_yaml::from_str(&meta_text).context("parse transcript meta")?;
        Ok((text, meta))
    }

    fn write_summary(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()> {
        atomic_write(&self.summary_md_path(trace_id), text.as_bytes())?;
        let yml = serde_yaml::to_string(meta).context("serialize summary meta")?;
        atomic_write(&self.summary_meta_path(trace_id), yml.as_bytes())
    }

    fn read_summary(&self, trace_id: &str) -> Result<(String, TraceMeta)> {
        let text_path = self.summary_md_path(trace_id);
        let meta_path = self.summary_meta_path(trace_id);
        let text = std::fs::read_to_string(&text_path).with_context(|| format!("read {}", text_path.display()))?;
        let meta_text = std::fs::read_to_string(&meta_path).with_context(|| format!("read {}", meta_path.display()))?;
        let meta: TraceMeta = serde_yaml::from_str(&meta_text).context("parse summary meta")?;
        Ok((text, meta))
    }

    fn write_rejection(&self, trace_id: &str, rec: &RejectionRecord) -> Result<()> {
        let yml = serde_yaml::to_string(rec).context("serialize rejection record")?;
        atomic_write(&self.rejection_path(trace_id), yml.as_bytes())
    }

    fn read_rejection(&self, trace_id: &str) -> Result<Option<RejectionRecord>> {
        let path = self.rejection_path(trace_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let rec: RejectionRecord = serde_yaml::from_str(&text).context("parse rejection record")?;
        Ok(Some(rec))
    }

    fn list_traces(&self, filter: &TraceFilter) -> Result<Vec<String>> {
        let mut matches = Vec::new();
        for root in self.trace_roots() {
            if !root.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&root).with_context(|| format!("read_dir {}", root.display()))? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let Some(trace_id) = name.to_str() else { continue };
                if !trace_matches(self, trace_id, filter)? {
                    continue;
                }
                matches.push(trace_id.to_string());
            }
        }
        Ok(matches)
    }

    fn has_trace(&self, trace_id: &str) -> Result<bool> {
        Ok(self.trace_dir(trace_id).is_dir() || self.envelope_path(trace_id).exists())
    }

    fn delete_trace(&self, trace_id: &str) -> Result<()> {
        let dir = self.trace_dir(trace_id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("remove_dir_all {}", dir.display()))?;
        }
        Ok(())
    }
}

fn trace_matches(store: &FsArtifactStore, trace_id: &str, filter: &TraceFilter) -> Result<bool> {
    let envelope = match store.read_envelope(trace_id) {
        Ok(e) => e,
        Err(_) => return Ok(false),
    };
    if let Some(kind) = filter.kind
        && envelope.kind != kind
    {
        return Ok(false);
    }
    if let Some(method) = filter.method
        && envelope.method != method
    {
        return Ok(false);
    }
    if let Some(domain) = &filter.domain {
        let fetched_meta = store.read_fetched(trace_id).ok().flatten();
        let matches_domain = fetched_meta
            .as_ref()
            .and_then(|(_, m)| url::Url::parse(&m.source).ok())
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
            .map(|h| h == domain.to_ascii_lowercase() || h.ends_with(&format!(".{}", domain.to_ascii_lowercase())))
            .unwrap_or(false);
        if !matches_domain {
            return Ok(false);
        }
    }
    if filter.rejected_only && store.read_rejection(trace_id)?.is_none() {
        return Ok(false);
    }
    if let Some(since) = &filter.since {
        let since_dt = parse_time(since).context("parse since bound")?;
        let received = parse_time(&envelope.received_at).context("parse envelope received-at")?;
        if received < since_dt {
            return Ok(false);
        }
    }
    if let Some(until) = &filter.until {
        let until_dt = parse_time(until).context("parse until bound")?;
        let received = parse_time(&envelope.received_at).context("parse envelope received-at")?;
        if received > until_dt {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| eyre::eyre!("invalid RFC3339 timestamp {s}: {e}"))
}

/// Helper constructor used by sites that only know the `IngestMethod` and need
/// a default envelope. Primarily for tests and Stage-0 scaffolding.
pub fn new_envelope(trace_id: &str, kind: IngestKind, method: IngestMethod) -> Envelope {
    Envelope {
        trace: trace_id.to_string(),
        kind,
        method,
        received_at: Utc::now().to_rfc3339(),
        origin_message_id: None,
        extra: HashMap::new(),
    }
}

/// In-memory `ArtifactStore`. Used by tests and by the `NoNetworkFetcher` stub
/// to exercise extractor logic without touching the filesystem.
#[derive(Debug, Default)]
pub struct MemArtifactStore {
    inner: Mutex<MemInner>,
}

#[derive(Debug, Default)]
struct MemInner {
    traces: HashMap<String, MemTrace>,
}

#[derive(Debug, Default)]
struct MemTrace {
    envelope: Option<Envelope>,
    body: Option<Vec<u8>>,
    attachments: HashMap<String, Vec<u8>>,
    fetched: Option<(Vec<u8>, FetchMeta)>,
    transcript: Option<(String, TraceMeta)>,
    summary: Option<(String, TraceMeta)>,
    rejection: Option<RejectionRecord>,
}

impl MemArtifactStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn missing(kind: &str, trace_id: &str) -> eyre::Report {
    eyre::eyre!("no {kind} recorded for trace {trace_id}")
}

impl ArtifactStore for MemArtifactStore {
    fn write_envelope(&self, trace_id: &str, envelope: &Envelope) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.envelope = Some(envelope.clone());
        Ok(())
    }

    fn read_envelope(&self, trace_id: &str) -> Result<Envelope> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        inner
            .traces
            .get(trace_id)
            .and_then(|t| t.envelope.clone())
            .ok_or_else(|| missing("envelope", trace_id))
    }

    fn write_body(&self, trace_id: &str, bytes: &[u8]) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.body = Some(bytes.to_vec());
        Ok(())
    }

    fn read_body(&self, trace_id: &str) -> Result<Vec<u8>> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        inner
            .traces
            .get(trace_id)
            .and_then(|t| t.body.clone())
            .ok_or_else(|| missing("body", trace_id))
    }

    fn write_attachment(&self, trace_id: &str, filename: &str, bytes: &[u8]) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.attachments.insert(filename.to_string(), bytes.to_vec());
        Ok(())
    }

    fn write_fetched(&self, trace_id: &str, bytes: &[u8], meta: &FetchMeta) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.fetched = Some((bytes.to_vec(), meta.clone()));
        Ok(())
    }

    fn read_fetched(&self, trace_id: &str) -> Result<Option<(Vec<u8>, FetchMeta)>> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        Ok(inner.traces.get(trace_id).and_then(|t| t.fetched.clone()))
    }

    fn read_raw(&self, trace_id: &str) -> Result<RawCapture> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        let trace = inner.traces.get(trace_id).ok_or_else(|| missing("trace", trace_id))?;
        let envelope = trace.envelope.clone().ok_or_else(|| missing("envelope", trace_id))?;
        Ok(RawCapture {
            envelope,
            body: trace.body.clone().unwrap_or_default(),
            attachments: trace.attachments.clone(),
            fetched: trace.fetched.clone(),
        })
    }

    fn write_transcript(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.transcript = Some((text.to_string(), meta.clone()));
        Ok(())
    }

    fn read_transcript(&self, trace_id: &str) -> Result<(String, TraceMeta)> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        inner
            .traces
            .get(trace_id)
            .and_then(|t| t.transcript.clone())
            .ok_or_else(|| missing("transcript", trace_id))
    }

    fn write_summary(&self, trace_id: &str, text: &str, meta: &TraceMeta) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.summary = Some((text.to_string(), meta.clone()));
        Ok(())
    }

    fn read_summary(&self, trace_id: &str) -> Result<(String, TraceMeta)> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        inner
            .traces
            .get(trace_id)
            .and_then(|t| t.summary.clone())
            .ok_or_else(|| missing("summary", trace_id))
    }

    fn write_rejection(&self, trace_id: &str, rec: &RejectionRecord) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        let entry = inner.traces.entry(trace_id.to_string()).or_default();
        entry.rejection = Some(rec.clone());
        Ok(())
    }

    fn read_rejection(&self, trace_id: &str) -> Result<Option<RejectionRecord>> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        Ok(inner.traces.get(trace_id).and_then(|t| t.rejection.clone()))
    }

    fn list_traces(&self, filter: &TraceFilter) -> Result<Vec<String>> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        let mut out = Vec::new();
        for (trace_id, trace) in &inner.traces {
            let Some(envelope) = &trace.envelope else { continue };
            if let Some(kind) = filter.kind
                && envelope.kind != kind
            {
                continue;
            }
            if let Some(method) = filter.method
                && envelope.method != method
            {
                continue;
            }
            if let Some(domain) = &filter.domain {
                let domain_lc = domain.to_ascii_lowercase();
                let host = trace
                    .fetched
                    .as_ref()
                    .and_then(|(_, m)| url::Url::parse(&m.source).ok())
                    .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()));
                let matches = host
                    .map(|h| h == domain_lc || h.ends_with(&format!(".{domain_lc}")))
                    .unwrap_or(false);
                if !matches {
                    continue;
                }
            }
            if filter.rejected_only && trace.rejection.is_none() {
                continue;
            }
            if let Some(since) = &filter.since {
                let since_dt = parse_time(since)?;
                let received = parse_time(&envelope.received_at)?;
                if received < since_dt {
                    continue;
                }
            }
            if let Some(until) = &filter.until {
                let until_dt = parse_time(until)?;
                let received = parse_time(&envelope.received_at)?;
                if received > until_dt {
                    continue;
                }
            }
            out.push(trace_id.clone());
        }
        Ok(out)
    }

    fn has_trace(&self, trace_id: &str) -> Result<bool> {
        let inner = self.inner.lock().expect("mem artifact store poisoned");
        Ok(inner.traces.contains_key(trace_id))
    }

    fn delete_trace(&self, trace_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("mem artifact store poisoned");
        inner.traces.remove(trace_id);
        Ok(())
    }
}

/// Compute the SHA-256 digest of arbitrary bytes and format it as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Parse `retention_days` as a `chrono::Duration` with bounds clamping.
pub fn retention_window(days: u32) -> chrono::Duration {
    let days = days.min(3650) as i64;
    chrono::Duration::days(days)
}

/// Ensure a new trace ID can safely own its directory. Fails if a directory
/// already exists (trace-id collision; callers should regenerate).
pub fn ensure_trace_dir_available<S: ArtifactStore>(store: &S, trace_id: &str) -> Result<()> {
    if store.has_trace(trace_id)? {
        bail!("trace {trace_id} already exists in artifact store");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
