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

    /// Raw extracted text the distiller received as input. Preserved for
    /// kinds whose published note is the only persistent source (Image,
    /// VoiceNote, Idea, Vocabulary) so the verbatim content is searchable
    /// in Obsidian months later. URL kinds (Article, Repo, Video, Thread)
    /// leave this `None` because the origin URL is the recoverable archive.
    ///
    /// Rendered by `distillers::render` as a `## Transcript` body section
    /// when `Some`. Indexed via the existing FTS5 `body` column (no new
    /// schema column required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// One claim extracted from the source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Claim {
    /// The claim text. Single sentence preferred; multi-sentence allowed.
    pub text: String,

    /// Optional anchor pointing back into the source. For YouTube this is
    /// "12:34" or "752s"; for articles, an anchor or section heading; for
    /// threads, a tweet ID. None when no precise anchor is available.
    #[serde(default)]
    pub anchor: Option<String>,
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
