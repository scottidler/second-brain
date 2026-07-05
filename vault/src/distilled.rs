//! Structured contract produced by Stage 2 extractors.
//!
//! Replaces the freeform `summary.md` artifact with a typed payload every
//! source-type extractor agrees on. Stage 3 publish renders it into the
//! vault markdown file; `index_vault` parses the markdown back into the
//! FTS5 index. The vault file remains the canonical store.

use serde::{Deserialize, Serialize};

/// Single structured contract every Stage 2 extractor produces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Distilled {
    /// 2-4 sentence prose summary. Used by FTS5, embeddings (Doc 2), and
    /// human display.
    pub summary: String,

    /// Structured claims extracted from the source. Order is significant
    /// (chronological for YouTube/Thread, narrative for articles).
    #[serde(default)]
    pub claims: Vec<Claim>,

    /// Canonical tags applied by the extractor, post-filtered against
    /// `canonical-tags.yml`. Max 7. Empty if the extractor doesn't tag.
    #[serde(default)]
    pub tags: Vec<String>,

    /// Outbound links discovered in the source content. Distinct from
    /// `source:` (the note's origin URL).
    #[serde(default)]
    pub links: Vec<Link>,

    /// Per-kind structured payload. Articles and Ideas leave this None;
    /// GitHub, YouTube, and Thread populate it with kind-specific data
    /// (stars, timestamps, thread author chain, etc.).
    #[serde(default)]
    pub kind_specific: Option<KindPayload>,

    /// Extractor metadata for debugging and replay.
    pub meta: DistilledMeta,

    /// Raw extracted text the distiller received as input. Preserved for kinds
    /// whose published note is the only persistent source (Image, VoiceNote,
    /// Idea, Vocabulary) so the verbatim content is searchable in Obsidian
    /// months later, AND — as of Phase B2 — for Video and Thread, whose
    /// transcripts power chunked semantic recall (regression-guarded; do not
    /// revert to `None`). Article and Repo still leave this `None`: the fetched
    /// markdown / origin URL is the recoverable archive.
    ///
    /// Rendered by `distillers::render` as a `## Transcript` body section
    /// when `Some`. Indexed via the existing FTS5 `body` column (no new
    /// schema column required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// The epistemic kind of a claim. Governs render decoration (a `**kind**`
/// prefix for every kind except `Fact`) and, downstream, cortex's
/// entity/triple extraction weighting.
///
/// This is the single source of truth for the claim vocabulary — consumers
/// import it, they never re-string the values. New values are added here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimKind {
    /// A factual assertion. The default, so legacy `distilled.yml` artifacts
    /// (no `kind:` field) and fact claims render with the exact pre-Phase-3
    /// visual shape (no `**kind**` prefix).
    #[default]
    Fact,
    /// An opinion / stance / argument the source advances. Captured attributed
    /// (see `Claim.who`) rather than dropped.
    Position,
    /// An actionable suggestion.
    Recommendation,
    /// A quantitative datum.
    Number,
}

impl ClaimKind {
    /// Canonical lowercase string form. Matches the serde representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Position => "position",
            Self::Recommendation => "recommendation",
            Self::Number => "number",
        }
    }

    /// Parse a known kind string, case-insensitively. Returns `None` for an
    /// unknown value so callers (e.g. the markdown parser) can decline to
    /// strip a bold token that is not actually a claim-kind decoration. The
    /// deserialize path, by contrast, maps unknown values to `Fact` with a
    /// WARN — see the `Deserialize` impl.
    pub fn parse_known(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fact" => Some(Self::Fact),
            "position" => Some(Self::Position),
            "recommendation" => Some(Self::Recommendation),
            "number" => Some(Self::Number),
            _ => None,
        }
    }
}

/// Forward-compatible deserialization (panel condition): an unknown `kind:`
/// string from a drifting LLM must NOT hard-fail the parse of the whole
/// `Distilled`. One bad enum value would otherwise demote an entire
/// distillation to the `yaml-parse-error` fallback path. Unknown values map to
/// `Fact` with a WARN so the datum survives (as a plain fact) and the operator
/// still sees the drift in the logs.
impl<'de> Deserialize<'de> for ClaimKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match Self::parse_known(&raw) {
            Some(kind) => kind,
            None => {
                log::warn!("ClaimKind::deserialize: unknown claim kind {raw:?}; defaulting to fact");
                Self::Fact
            }
        })
    }
}

/// One claim extracted from the source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Claim {
    /// The claim text. Single sentence preferred; multi-sentence allowed.
    pub text: String,

    /// Optional anchor pointing back into the source. For YouTube this is
    /// "12:34" or "752s"; for articles, an anchor or section heading; for
    /// threads, a tweet ID. None when no precise anchor is available.
    #[serde(default)]
    pub anchor: Option<String>,

    /// The epistemic kind of the claim. `#[serde(default)]` (via
    /// `ClaimKind::default() == Fact`) keeps legacy `distilled.yml` artifacts
    /// without the field deserializable and unchanged.
    #[serde(default)]
    pub kind: ClaimKind,

    /// Attribution for positions / thread claims: "@simonw", "the author".
    /// None when the source does not attribute the claim.
    #[serde(default)]
    pub who: Option<String>,

    /// Short verbatim quote (≤200 chars) supporting the claim. Rendered as an
    /// indented blockquote line beneath the claim bullet when present.
    #[serde(default)]
    pub quote: Option<String>,
}

/// An outbound link discovered in the source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Link {
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// Per-kind structured payload. The `kind` tag selects the variant on
/// deserialization so YAML files are self-describing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum KindPayload {
    Repo(RepoPayload),
    Video(VideoPayload),
    Thread(ThreadPayload),
}

/// GitHub repository metadata frozen at ingest time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RepoPayload {
    #[serde(default)]
    pub stars: Option<u32>,
    #[serde(default)]
    pub primary_language: Option<String>,
    /// ISO 8601 UTC date, frozen at ingest.
    #[serde(default)]
    pub last_commit: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// Extracted install instructions, max ~500 chars.
    #[serde(default)]
    pub install: Option<String>,
}

/// YouTube video metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VideoPayload {
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
    /// ISO 8601 UTC date.
    #[serde(default)]
    pub published_at: Option<String>,
    /// `owner/repo` slugs harvested from the video description. Rendered as a
    /// top-level `github:` YAML sequence. `#[serde(default)]` keeps legacy
    /// `distilled.yml` artifacts without the field deserializable.
    #[serde(default)]
    pub repos: Vec<String>,
}

/// Thread metadata for X/Reddit/HN sources.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ThreadPayload {
    #[serde(default)]
    pub author: Option<String>,
    pub post_count: u32,
    /// Platform identifier: "x", "reddit", "hn".
    pub platform: String,
}

/// Extractor-side bookkeeping recorded for each distilled artifact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DistilledMeta {
    /// Pattern identifier + version (e.g. "distill-article-v1").
    pub extractor: String,
    /// Model identifier or sentinel ("timeout", "fabric-error", "yaml-parse-error").
    pub model: String,
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    /// ISO 8601 UTC, e.g. "2026-05-16T14:03:22Z".
    pub produced_at: String,
    /// Validation outcome for forensics and replay.
    #[serde(default)]
    pub validation: ValidationMeta,
}

/// Validation outcome attached to every distilled artifact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ValidationMeta {
    /// `None` when validation passed cleanly. Set to a stable error tag
    /// (e.g. "fabric-timeout", "yaml-parse-error", "missing-summary") when
    /// the distiller had to fall back.
    #[serde(default)]
    pub fallback_reason: Option<String>,
    /// Truncation events applied to bring the payload within bounds.
    /// Each entry is a short tag like "claims:10>7" or "summary:2840>2000".
    #[serde(default)]
    pub bounds_truncations: Vec<String>,
    /// Number of claim anchors stripped because they failed per-kind
    /// validation (e.g. timestamp outside `duration_seconds`).
    #[serde(default)]
    pub anchors_stripped: u32,
    /// Raw Fabric stdout, populated only on parse failure for forensics.
    #[serde(default)]
    pub raw_output: Option<String>,
}

#[cfg(test)]
mod tests;
