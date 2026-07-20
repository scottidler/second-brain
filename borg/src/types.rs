use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Input content classification - what did we receive?
/// Input sources construct this; the pipeline dispatches on it.
#[derive(Debug, Clone)]
pub enum ContentKind {
    /// A URL capture. `note` is the operator's capture annotation (the prose
    /// that accompanied the URL, first-URL token removed + whitespace
    /// collapsed); `None` for a bare-URL capture. Threaded to the published
    /// note's `capture-note:` frontmatter + `## Why Captured` section, and to
    /// the distiller as trusted-but-labeled context.
    Url {
        url: String,
        note: Option<String>,
    },
    Image {
        data: Vec<u8>,
        filename: String,
    },
    Pdf {
        data: Vec<u8>,
        filename: String,
    },
    Audio {
        data: Vec<u8>,
        filename: String,
    },
    Text(String),
    Document {
        data: Vec<u8>,
        filename: String,
    },
    /// A clyde session/thread export, pre-fetched by the harvest reader
    /// (harvest-clyde-sessions design). `body` is the concatenated
    /// role-labeled transcript text for every member session (Phase 3's
    /// `watermark::thread_body_text`). `members` are the bulk-metadata
    /// records (repo, scope, title, duration, redaction-count, dates) for
    /// every session in the thread, in `created` order - carried WITHOUT
    /// their own `body` field (the harvest publish runner fetches transcript
    /// bodies once, for `body`, rather than storing them twice). `primary_id`
    /// names which member anchors `source:`/`repo:`/the watermark entry (the
    /// most-messages session, design doc: Selection > Thread boundary
    /// rules). `body_truncated` is true when ANY member's `--with-body` fetch
    /// flagged clyde-side truncation - drives the `[TRANSCRIPT TRUNCATED]`
    /// marker so truncation is never silent to the model.
    Session {
        body: String,
        members: Vec<crate::harvest::contract::SessionRecord>,
        primary_id: String,
        body_truncated: bool,
    },
}

/// Content-kind classification for staged pipeline dispatch.
/// Unlike `ContentKind`, this is a processing hint attached to a Stage-0
/// sidecar (it does not carry payload bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IngestKind {
    ArticleUrl,
    GitHubUrl,
    YoutubeUrl,
    ThreadUrl,
    Image,
    VoiceNote,
    Idea,
    VocabularyEn,
    VocabularyEs,
    /// A clyde session/thread selected by `sb borg harvest`
    /// (harvest-clyde-sessions design).
    Session,
}

impl fmt::Display for IngestKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArticleUrl => write!(f, "article-url"),
            Self::GitHubUrl => write!(f, "github-url"),
            Self::YoutubeUrl => write!(f, "youtube-url"),
            Self::ThreadUrl => write!(f, "thread-url"),
            Self::Image => write!(f, "image"),
            Self::VoiceNote => write!(f, "voice-note"),
            Self::Idea => write!(f, "idea"),
            Self::VocabularyEn => write!(f, "vocabulary-en"),
            Self::VocabularyEs => write!(f, "vocabulary-es"),
            Self::Session => write!(f, "session"),
        }
    }
}

/// Which stage an artifact or a gate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageKind {
    Raw,
    Transcript,
    Summary,
    Publish,
}

impl fmt::Display for StageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw => write!(f, "raw"),
            Self::Transcript => write!(f, "transcript"),
            Self::Summary => write!(f, "summary"),
            Self::Publish => write!(f, "publish"),
        }
    }
}

/// Which gate fired (0/1/2/3 per the design doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateId {
    /// URL blocklist enforcement (Stage 0, pre-fetch).
    DomainBlocklist,
    /// Block-page detection on raw fetched bytes (Stage 1, pre-extract).
    BlockPage,
    /// Failed-fetch paraphrase detection on the summary (Stage 2 backstop).
    FailedFetchParaphrase,
    /// Structural quality (word count, summary section, etc.) on the final note.
    StructuralQuality,
    /// Harvest's selection gate (`sb borg harvest`, harvest-clyde-sessions
    /// design): scores a clyde session candidate and rejects those below the
    /// selection bar. The real gate for sessions - Gate-0 (domain blocklist)
    /// is a structural no-op for this source.
    Selection,
    /// Harvest's per-record PARSE gate (`sb borg harvest`, harvest-completion
    /// design Phase 1): a single `sessions[]` element that failed contract
    /// deserialization is skipped and receipted here (rather than aborting the
    /// whole batch), keyed by the `session-id` recovered from the malformed
    /// element. The durable-skip defense against a future clyde contract drift.
    Parse,
}

impl fmt::Display for GateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DomainBlocklist => write!(f, "domain-blocklist"),
            Self::BlockPage => write!(f, "block-page"),
            Self::FailedFetchParaphrase => write!(f, "failed-fetch-paraphrase"),
            Self::StructuralQuality => write!(f, "structural-quality"),
            Self::Selection => write!(f, "selection"),
            Self::Parse => write!(f, "parse"),
        }
    }
}

/// Envelope sidecar written at Stage 0: transport metadata for the capture event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Envelope {
    pub trace: String,
    pub kind: IngestKind,
    pub method: IngestMethod,
    pub received_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_message_id: Option<String>,
    /// Transport-specific fields (chat_id, from_user, reply_to, etc.) preserved verbatim.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_yaml::Value>,
}

/// Fetch sidecar written at Stage 0 when a URL was fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FetchMeta {
    pub source: String,
    pub extractor: String,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks_attempted: Vec<String>,
    /// Byline surfaced by a fetcher that could see the source markup
    /// (`BrowserUaFetcher` via `byline::extract`, or a future Jina-JSON path).
    /// `None` on the `fabric -u` default path, which exposes no HTML.
    /// Additive + `Option`: existing impls default it to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// Result of a Stage-0 network fetch.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub bytes: Vec<u8>,
    pub meta: FetchMeta,
}

/// Transcript sidecar written at Stage 1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct TraceMeta {
    pub extractor: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks_attempted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The full raw capture event as persisted at Stage 0 and read by Stage 1.
/// Any Stage-1 extractor must produce a transcript from only this on-disk view.
#[derive(Debug, Clone)]
pub struct RawCapture {
    pub envelope: Envelope,
    /// Message body (caption / prose / idea / vocab input). Always present, may be empty.
    pub body: Vec<u8>,
    /// Binary attachments by filename (image, audio, pdf, etc.).
    pub attachments: HashMap<String, Vec<u8>>,
    /// Fetched URL response body + metadata, when the capture referenced a URL.
    pub fetched: Option<(Vec<u8>, FetchMeta)>,
}

/// Output of a Stage-1 extractor: text + metadata about how it was produced.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub meta: TraceMeta,
}

/// Selector used by `ArtifactStore::list_traces` and `borg replay`.
#[derive(Debug, Clone, Default)]
pub struct TraceFilter {
    pub kind: Option<IngestKind>,
    pub method: Option<IngestMethod>,
    pub domain: Option<String>,
    /// Only list rejected traces.
    pub rejected_only: bool,
    /// Lower bound on envelope `received-at` (RFC3339).
    pub since: Option<String>,
    /// Upper bound on envelope `received-at` (RFC3339).
    pub until: Option<String>,
}

/// A gate rejection record. Written to `<trace_id>/rejection.yml` in per-trace layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RejectionRecord {
    pub trace: String,
    pub stage: StageKind,
    pub gate: GateId,
    pub reason: String,
    pub rejected_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default)]
    pub blocklist_updated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retriable_after: Option<String>,
}

/// Borg's ingest method IS `vault::schema::Method` (schema-is-law). The old
/// parallel `IngestMethod` enum existed only because borg didn't enable
/// vault's `schemars` feature; now that it does (see Cargo.toml), the shadow
/// is gone and this alias keeps every `IngestMethod::Telegram` call site
/// working. `Method` additionally carries a `Manual` variant.
pub use vault::schema::Method as IngestMethod;

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptionRequest {
    pub audio_bytes: Vec<u8>,
    pub language: Option<String>,
    pub format: AudioFormat,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AudioFormat {
    Mp3,
    Wav,
    Ogg,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
    pub language: String,
    pub duration_secs: f64,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IngestRequest {
    pub url: String,
    pub tags: Option<Vec<String>>,
    pub priority: Option<Priority>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub method: Option<IngestMethod>,
    /// Operator capture annotation accompanying the URL (Phase 8). Additive +
    /// optional: existing extension bodies that omit it deserialize unchanged
    /// (`extension_body_matches_ingest_request` enforces this). Rendered into
    /// the published note's `## Why Captured` section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Priority {
    Normal,
    High,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IngestResult {
    pub status: IngestStatus,
    pub note_path: Option<String>,
    pub title: Option<String>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<IngestMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obsidian_url: Option<String>,
    /// Typed failure classification for a `Failed` result, set at the point
    /// the failure occurs (intake reject, classify, fetch, quality gate,
    /// publish, timeout). The terminal receipts write reads this directly
    /// instead of substring-matching the free-form `reason`. `None` on a
    /// non-failure result, or a failure whose site did not classify it (the
    /// terminal write then defaults to `FetchFailed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<vault::receipts::FailureStage>,
    /// True when the published note came from a distill FALLBACK (the
    /// distiller degraded gracefully instead of producing a clean structured
    /// artifact). Recorded on the receipts success row so `sb borg log
    /// --degraded` can surface these; replaces the retired halt-on-hard-distill
    /// policy.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub enum IngestStatus {
    #[default]
    Queued,
    Completed,
    Duplicate {
        original_date: String,
    },
    Failed {
        reason: String,
    },
}

#[cfg(test)]
mod tests;
