use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestRequest {
    pub url: String,
    pub tags: Option<Vec<String>>,
    pub priority: Option<Priority>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub method: Option<IngestMethod>,
}

#[derive(Debug, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "folder")]
    pub domain: Option<String>,
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
