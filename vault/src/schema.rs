use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Frontmatter keys owned by cortex. Borg preserves these across reingest so a
/// fetch of an already-classified URL does not strip the classification work.
/// Single source of truth; do not duplicate this list.
pub const CORTEX_PRESERVE_KEYS: &[&str] = &[
    "domain",
    "status",
    "cortex-classified",
    "cortex-classified-by",
    "cortex-confidence",
    "cortex-quality",
    "cortex-quality-issues",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Domain {
    Ai,
    Tech,
    Football,
    Work,
    Writing,
    Music,
    Spanish,
    Life,
    Homelab,
    Diy,
    Resources,
    System,
}

impl Domain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Tech => "tech",
            Self::Football => "football",
            Self::Work => "work",
            Self::Writing => "writing",
            Self::Music => "music",
            Self::Spanish => "spanish",
            Self::Life => "life",
            Self::Homelab => "homelab",
            Self::Diy => "diy",
            Self::Resources => "resources",
            Self::System => "system",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Ai,
            Self::Tech,
            Self::Football,
            Self::Work,
            Self::Writing,
            Self::Music,
            Self::Spanish,
            Self::Life,
            Self::Homelab,
            Self::Diy,
            Self::Resources,
            Self::System,
        ]
    }
}

impl fmt::Display for Domain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Domain {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ai" => Ok(Self::Ai),
            "tech" => Ok(Self::Tech),
            "football" => Ok(Self::Football),
            "work" => Ok(Self::Work),
            "writing" => Ok(Self::Writing),
            "music" => Ok(Self::Music),
            "spanish" => Ok(Self::Spanish),
            "life" => Ok(Self::Life),
            "knowledge" => Ok(Self::Life), // backwards-compat alias
            "homelab" => Ok(Self::Homelab),
            "diy" => Ok(Self::Diy),
            "resources" => Ok(Self::Resources),
            "system" => Ok(Self::System),
            _ => Err(format!("unknown domain: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum NoteType {
    Youtube,
    Article,
    Github,
    Social,
    Reddit,
    Image,
    Pdf,
    Audio,
    Note,
    Vocab,
    Document,
    Code,
    Book,
    Video,
    Research,
    Daily,
    Meeting,
    Moc,
    Link,
    Poem,
    System,
}

impl NoteType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Youtube => "youtube",
            Self::Article => "article",
            Self::Github => "github",
            Self::Social => "social",
            Self::Reddit => "reddit",
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Audio => "audio",
            Self::Note => "note",
            Self::Vocab => "vocab",
            Self::Document => "document",
            Self::Code => "code",
            Self::Book => "book",
            Self::Video => "video",
            Self::Research => "research",
            Self::Daily => "daily",
            Self::Meeting => "meeting",
            Self::Moc => "moc",
            Self::Link => "link",
            Self::Poem => "poem",
            Self::System => "system",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Youtube,
            Self::Article,
            Self::Github,
            Self::Social,
            Self::Reddit,
            Self::Image,
            Self::Pdf,
            Self::Audio,
            Self::Note,
            Self::Vocab,
            Self::Document,
            Self::Code,
            Self::Book,
            Self::Video,
            Self::Research,
            Self::Daily,
            Self::Meeting,
            Self::Moc,
            Self::Link,
            Self::Poem,
            Self::System,
        ]
    }

    /// Note kinds whose published body carries a `## Transcript` section that
    /// Phase B chunks and embeds. Drives the SQL `note_type IN (...)` filter
    /// in `vault::search::vector::stale_embedding_targets` so it can never
    /// drift from the actual enum strings. Adding a new transcript-bearing
    /// kind means adding a variant here, not editing SQL.
    ///
    /// The conceptual buckets from the hybrid-retrieval design doc map to
    /// these enum variants:
    /// - Image            -> `Image`
    /// - VoiceNote / Audio -> `Audio`
    /// - Idea             -> `Note`
    /// - Vocabulary       -> `Vocab`
    /// - Video            -> `Video`
    /// - Thread           -> `Social` (X/Twitter) and `Reddit`
    pub fn transcript_eligible() -> &'static [Self] {
        &[
            Self::Image,
            Self::Audio,
            Self::Note,
            Self::Vocab,
            Self::Video,
            Self::Social,
            Self::Reddit,
        ]
    }
}

impl fmt::Display for NoteType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for NoteType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "youtube" => Ok(Self::Youtube),
            "article" => Ok(Self::Article),
            "github" => Ok(Self::Github),
            "social" => Ok(Self::Social),
            "reddit" => Ok(Self::Reddit),
            "image" => Ok(Self::Image),
            "pdf" => Ok(Self::Pdf),
            "audio" => Ok(Self::Audio),
            "note" => Ok(Self::Note),
            "vocab" => Ok(Self::Vocab),
            "document" => Ok(Self::Document),
            "code" => Ok(Self::Code),
            "book" => Ok(Self::Book),
            "video" => Ok(Self::Video),
            "research" => Ok(Self::Research),
            "daily" => Ok(Self::Daily),
            "meeting" => Ok(Self::Meeting),
            "moc" => Ok(Self::Moc),
            "link" => Ok(Self::Link),
            "poem" => Ok(Self::Poem),
            "system" => Ok(Self::System),
            _ => Err(format!("unknown note type: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Authored,
    Assisted,
    Generated,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Assisted => "assisted",
            Self::Generated => "generated",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Authored, Self::Assisted, Self::Generated]
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Origin {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "authored" => Ok(Self::Authored),
            "assisted" => Ok(Self::Assisted),
            "generated" => Ok(Self::Generated),
            _ => Err(format!("unknown origin: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Unread,
    Reading,
    Reviewed,
    Starred,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unread => "unread",
            Self::Reading => "reading",
            Self::Reviewed => "reviewed",
            Self::Starred => "starred",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Unread, Self::Reading, Self::Reviewed, Self::Starred]
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Status {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "unread" => Ok(Self::Unread),
            "reading" => Ok(Self::Reading),
            "reviewed" => Ok(Self::Reviewed),
            "starred" => Ok(Self::Starred),
            _ => Err(format!("unknown status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Telegram,
    Discord,
    Http,
    Clipboard,
    Cli,
    Ntfy,
    Signal,
    Manual,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Discord => "discord",
            Self::Http => "http",
            Self::Clipboard => "clipboard",
            Self::Cli => "cli",
            Self::Ntfy => "ntfy",
            Self::Signal => "signal",
            Self::Manual => "manual",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Telegram,
            Self::Discord,
            Self::Http,
            Self::Clipboard,
            Self::Cli,
            Self::Ntfy,
            Self::Signal,
            Self::Manual,
        ]
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Method {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "telegram" => Ok(Self::Telegram),
            "discord" => Ok(Self::Discord),
            "http" => Ok(Self::Http),
            "clipboard" => Ok(Self::Clipboard),
            "cli" => Ok(Self::Cli),
            "ntfy" => Ok(Self::Ntfy),
            "signal" => Ok(Self::Signal),
            "manual" => Ok(Self::Manual),
            _ => Err(format!("unknown method: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_roundtrip() {
        for d in Domain::all() {
            let s = d.as_str();
            let parsed: Domain = s.parse().expect("should parse");
            assert_eq!(*d, parsed);
        }
    }

    #[test]
    fn test_domain_serde_roundtrip() {
        for d in Domain::all() {
            let json = serde_json::to_string(d).expect("serialize");
            let parsed: Domain = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*d, parsed);
        }
    }

    #[test]
    fn test_note_type_roundtrip() {
        for t in NoteType::all() {
            let s = t.as_str();
            let parsed: NoteType = s.parse().expect("should parse");
            assert_eq!(*t, parsed);
        }
    }

    #[test]
    fn test_note_type_serde_roundtrip() {
        for t in NoteType::all() {
            let json = serde_json::to_string(t).expect("serialize");
            let parsed: NoteType = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*t, parsed);
        }
    }

    #[test]
    fn test_origin_roundtrip() {
        for o in Origin::all() {
            let s = o.as_str();
            let parsed: Origin = s.parse().expect("should parse");
            assert_eq!(*o, parsed);
        }
    }

    #[test]
    fn test_status_roundtrip() {
        for s in Status::all() {
            let str_val = s.as_str();
            let parsed: Status = str_val.parse().expect("should parse");
            assert_eq!(*s, parsed);
        }
    }

    #[test]
    fn test_method_roundtrip() {
        for m in Method::all() {
            let s = m.as_str();
            let parsed: Method = s.parse().expect("should parse");
            assert_eq!(*m, parsed);
        }
    }

    #[test]
    fn test_method_includes_manual() {
        assert!(Method::all().contains(&Method::Manual));
    }

    #[test]
    fn test_domain_display() {
        assert_eq!(Domain::Ai.to_string(), "ai");
        assert_eq!(Domain::Football.to_string(), "football");
    }

    #[test]
    fn test_domain_case_insensitive_parse() {
        assert_eq!("AI".parse::<Domain>(), Ok(Domain::Ai));
        assert_eq!("Tech".parse::<Domain>(), Ok(Domain::Tech));
        assert_eq!("FOOTBALL".parse::<Domain>(), Ok(Domain::Football));
    }

    #[test]
    fn test_unknown_domain_errors() {
        assert!("bogus".parse::<Domain>().is_err());
    }

    #[test]
    fn test_unknown_note_type_errors() {
        assert!("blogpost".parse::<NoteType>().is_err());
    }
}
