use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Input content classification - what did we receive?
/// Input sources construct this; the pipeline dispatches on it.
#[derive(Debug, Clone)]
pub enum ContentKind {
    Url(String),
    Image { data: Vec<u8>, filename: String },
    Pdf { data: Vec<u8>, filename: String },
    Audio { data: Vec<u8>, filename: String },
    Text(String),
    Document { data: Vec<u8>, filename: String },
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
}

impl fmt::Display for GateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DomainBlocklist => write!(f, "domain-blocklist"),
            Self::BlockPage => write!(f, "block-page"),
            Self::FailedFetchParaphrase => write!(f, "failed-fetch-paraphrase"),
            Self::StructuralQuality => write!(f, "structural-quality"),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum IngestMethod {
    Telegram,
    Discord,
    Http,
    Clipboard,
    Cli,
    Ntfy,
}

impl fmt::Display for IngestMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Telegram => write!(f, "telegram"),
            Self::Discord => write!(f, "discord"),
            Self::Http => write!(f, "http"),
            Self::Clipboard => write!(f, "clipboard"),
            Self::Cli => write!(f, "cli"),
            Self::Ntfy => write!(f, "ntfy"),
        }
    }
}

impl From<IngestMethod> for vault::schema::Method {
    fn from(m: IngestMethod) -> Self {
        match m {
            IngestMethod::Telegram => Self::Telegram,
            IngestMethod::Discord => Self::Discord,
            IngestMethod::Http => Self::Http,
            IngestMethod::Clipboard => Self::Clipboard,
            IngestMethod::Cli => Self::Cli,
            IngestMethod::Ntfy => Self::Ntfy,
        }
    }
}

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
mod tests {
    use super::*;

    #[test]
    fn test_transcription_request_roundtrip() {
        let req = TranscriptionRequest {
            audio_bytes: vec![1, 2, 3],
            language: Some("en".to_string()),
            format: AudioFormat::Mp3,
        };
        let json = serde_yaml::to_string(&req).expect("serialize");
        let deserialized: TranscriptionRequest = serde_yaml::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.audio_bytes, vec![1, 2, 3]);
        assert_eq!(deserialized.language, Some("en".to_string()));
    }

    #[test]
    fn test_ingest_request_roundtrip() {
        let req = IngestRequest {
            url: "https://youtube.com/watch?v=abc".to_string(),
            tags: Some(vec!["ai".to_string(), "rust".to_string()]),
            priority: Some(Priority::High),
            force: false,
            method: Some(IngestMethod::Clipboard),
        };
        let json = serde_yaml::to_string(&req).expect("serialize");
        let deserialized: IngestRequest = serde_yaml::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.url, "https://youtube.com/watch?v=abc");
        assert_eq!(deserialized.tags, Some(vec!["ai".to_string(), "rust".to_string()]));
    }

    #[test]
    fn test_content_kind_url() {
        let kind = ContentKind::Url("https://example.com".to_string());
        assert!(matches!(kind, ContentKind::Url(ref u) if u == "https://example.com"));
    }

    #[test]
    fn test_content_kind_image() {
        let kind = ContentKind::Image {
            data: vec![1, 2, 3],
            filename: "test.png".to_string(),
        };
        assert!(matches!(kind, ContentKind::Image { ref filename, .. } if filename == "test.png"));
    }

    #[test]
    fn test_content_kind_text() {
        let kind = ContentKind::Text("hello world".to_string());
        assert!(matches!(kind, ContentKind::Text(ref t) if t == "hello world"));
    }

    #[test]
    fn test_ingest_result_with_failed_status() {
        let result = IngestResult {
            status: IngestStatus::Failed {
                reason: "network error".to_string(),
            },
            note_path: None,
            title: None,
            tags: vec![],
            ..Default::default()
        };
        let json = serde_yaml::to_string(&result).expect("serialize");
        let deserialized: IngestResult = serde_yaml::from_str(&json).expect("deserialize");
        match deserialized.status {
            IngestStatus::Failed { reason } => assert_eq!(reason, "network error"),
            _ => panic!("expected Failed status"),
        }
    }
}
