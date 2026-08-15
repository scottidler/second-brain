use chrono::Utc;
use chrono_tz::Tz;
use std::collections::BTreeMap;

use crate::config::FrontmatterConfig;
use crate::types::IngestMethod;

#[derive(Default)]
pub struct NoteContent {
    pub title: String,
    pub source_url: Option<String>,
    pub asset_path: Option<String>,
    pub tags: Vec<String>,
    pub summary: String,
    pub description: Option<String>,
    /// Operator capture annotation (Phase 8): the prose that accompanied the
    /// captured URL / the Signal attachment caption. Rendered verbatim as a
    /// `capture-note:` frontmatter key and a `## Why Captured` body section
    /// above `## Summary`. `None` for a bare capture (no annotation) so no
    /// empty key / empty section is emitted.
    pub capture_note: Option<String>,
    pub content_type: ContentType,
    pub embed_code: Option<String>,
    pub method: Option<IngestMethod>,
    pub trace_id: Option<String>,
    /// Vault-relative paths to slide JPEGs the note owns. Rendered into the
    /// `slides:` frontmatter list so cleanup on replay can find them.
    pub slides: Vec<String>,
    /// Post-Phase-6 cutover: pre-rendered structured body produced by
    /// `distillers::render`. When `Some`, replaces the legacy
    /// `## Summary\n\n{summary}` block - the rendered Distilled already
    /// carries `## Summary` / `## Claims` / `## Links` headings of its own.
    /// `None` for non-URL kinds (image, audio, vocab, idea) that still use
    /// the legacy prose-summary body.
    pub distilled_body: Option<String>,
    /// Additional frontmatter keys merged into the rendered YAML before the
    /// closing `---`. Populated by `distillers::render` with `distilled:
    /// true`, `distilled-extractor`, and per-kind `cortex-*` fields.
    pub frontmatter_additions: BTreeMap<String, serde_yaml::Value>,
    /// `origin:` override. `None` (the default for every existing kind)
    /// renders `origin: assisted`, unchanged from before this field existed.
    /// Harvest notes (produced end-to-end by the distiller, no operator
    /// capture) set `Some(Origin::Generated)` (design doc: Data Model).
    pub origin: Option<vault::schema::Origin>,
    /// `status:` frontmatter, omitted (as before) when `None`. Harvest notes
    /// set `Some(Status::Unread)` (design doc: Data Model); no other kind
    /// writes this field today.
    pub status: Option<vault::schema::Status>,
}

#[derive(Default)]
pub enum ContentType {
    YouTube {
        uploader: String,
        duration_secs: f64,
    },
    Article {
        /// Byline extracted from the fetched page, when a fetcher surfaced it.
        /// `None` on the `fabric -u` default path (no HTML to parse).
        author: Option<String>,
    },
    GitHub {
        /// Repo owner, parsed from the source URL at dispatch.
        owner: String,
    },
    Social,
    Reddit,
    Image {
        asset_path: String,
    },
    Pdf {
        asset_path: String,
    },
    Audio {
        asset_path: String,
        duration_secs: Option<f64>,
    },
    #[default]
    Note,
    VocabDefine {
        word: String,
        language: String,
    },
    VocabClarify {
        word_a: String,
        word_b: String,
        language: String,
    },
    Document {
        asset_path: String,
    },
    Code {
        language: String,
    },
    /// Distilled Claude Code session/thread note (harvest-clyde-sessions
    /// design). Fields land in Phase 5's pipeline handler; Phase 1 wires the
    /// `type:` frontmatter mapping only.
    Session,
}

/// Every frontmatter key [`render_note`] can emit from its own fields - the
/// SOURCE OF TRUTH for the borg-owned key policy that
/// `pipeline::session`'s replace-in-place merge is derived from (design doc
/// `2026-08-15-harvest-note-identity-trace-keyed-replace.md`, Data Model: the
/// borg-owned set is "DERIVED FROM THE WRITER, not hand-listed, so it cannot
/// drift the way `slug:` did").
///
/// Not every key appears on every note - most are conditional on a `Some`
/// field or a `ContentType` branch - but nothing outside this list is ever
/// written by this function's own field handling. Keys from
/// `NoteContent::frontmatter_additions` are deliberately NOT here: those are
/// the CALLER's keys, and each caller owns its own list.
///
/// `markdown::tests::render_note_keys_matches_the_writer` renders a matrix
/// covering every `ContentType` variant (via an exhaustive match, so a new
/// variant fails to compile until the matrix covers it) with every optional
/// field populated, and asserts the emitted key set equals this constant. Add
/// a key to `render_note` without adding it here and that test fails.
pub const RENDER_NOTE_KEYS: &[&str] = &[
    "title",
    "date",
    "ingested",
    "source",
    "asset",
    "type",
    "origin",
    "status",
    "method",
    "trace",
    "capture-note",
    "slides",
    "tags",
    "creator",
    "duration",
    "language",
];

pub fn render_note(note: &NoteContent, frontmatter_config: &FrontmatterConfig) -> String {
    let tz: Tz = frontmatter_config
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = Utc::now().with_timezone(&tz);
    let date = now.format("%Y-%m-%d").to_string();

    let mut all_tags = frontmatter_config.default_tags.clone();
    all_tags.extend(note.tags.clone());
    // Deduplicate
    all_tags.sort();
    all_tags.dedup();

    let tags_yaml = all_tags
        .iter()
        .map(|t| format!("  - {t}"))
        .collect::<Vec<_>>()
        .join("\n");

    // Map each ContentType to a vault::schema::NoteType, then render via
    // `as_str()` so the published `type:` can never drift from the schema.
    let type_field = match &note.content_type {
        ContentType::YouTube { .. } => vault::schema::NoteType::Youtube,
        ContentType::Article { .. } => vault::schema::NoteType::Article,
        ContentType::GitHub { .. } => vault::schema::NoteType::Github,
        ContentType::Social => vault::schema::NoteType::Social,
        ContentType::Reddit => vault::schema::NoteType::Reddit,
        ContentType::Image { .. } => vault::schema::NoteType::Image,
        ContentType::Pdf { .. } => vault::schema::NoteType::Pdf,
        ContentType::Audio { .. } => vault::schema::NoteType::Audio,
        ContentType::Note => vault::schema::NoteType::Note,
        ContentType::VocabDefine { .. } | ContentType::VocabClarify { .. } => vault::schema::NoteType::Vocab,
        ContentType::Document { .. } => vault::schema::NoteType::Document,
        ContentType::Code { .. } => vault::schema::NoteType::Code,
        ContentType::Session => vault::schema::NoteType::Session,
    }
    .as_str();

    let mut fm = format!(
        "---\ntitle: {}\ndate: {date}\ningested: {date}\n",
        yaml_scalar(&note.title),
    );

    if let Some(source) = &note.source_url {
        fm.push_str(&format!("source: {}\n", yaml_scalar(source)));
    }
    if let Some(asset) = &note.asset_path {
        fm.push_str(&format!("asset: {}\n", yaml_scalar(asset)));
    }
    fm.push_str(&format!("type: {type_field}\n"));
    let origin = note.origin.unwrap_or(vault::schema::Origin::Assisted);
    fm.push_str(&format!("origin: {}\n", origin.as_str()));
    if let Some(status) = note.status {
        fm.push_str(&format!("status: {}\n", status.as_str()));
    }

    if let Some(method) = &note.method {
        fm.push_str(&format!("method: {method}\n"));
    }

    if let Some(ref tid) = note.trace_id {
        fm.push_str(&format!("trace: {tid}\n"));
    }

    // Capture note (Phase 8): the operator's own annotation. Emitted only when
    // present and non-empty so a bare capture never writes an empty key.
    if let Some(capture) = note.capture_note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        fm.push_str(&format!("capture-note: {}\n", yaml_scalar(capture)));
    }

    if !note.slides.is_empty() {
        fm.push_str("slides:\n");
        for s in &note.slides {
            fm.push_str(&format!("  - {s}\n"));
        }
    }

    fm.push_str(&format!("tags:\n{tags_yaml}\n"));

    // `creator` is written exactly once: resolve the per-kind author via
    // `creator_for` (uploader / owner / article byline), falling back to a
    // non-empty `default_creator`. This is the single source of the
    // `creator:` line - the YouTube arm below no longer emits one, so a
    // YouTube note with a non-empty `default_creator` can never double-write.
    if let Some(creator) = creator_for(&note.content_type).or_else(|| {
        (!frontmatter_config.default_creator.is_empty()).then(|| frontmatter_config.default_creator.clone())
    }) {
        fm.push_str(&format!("creator: {}\n", yaml_scalar(&creator)));
    }

    match &note.content_type {
        ContentType::YouTube { duration_secs, .. } => {
            let minutes = (*duration_secs / 60.0).round() as u32;
            fm.push_str(&format!("duration: {minutes}\n"));
        }
        ContentType::Audio {
            duration_secs: Some(secs),
            ..
        } => {
            let minutes = (*secs / 60.0).round() as u32;
            fm.push_str(&format!("duration: {minutes}\n"));
        }
        ContentType::Code { language } => {
            fm.push_str(&format!("language: \"{language}\"\n"));
        }
        _ => {}
    }

    // Post-Phase-6 cutover: merge any frontmatter additions produced by
    // `distillers::render` (distilled flag, extractor id, per-kind
    // `cortex-*` keys). Sorted alphabetically for stable diffs.
    for (key, value) in &note.frontmatter_additions {
        fm.push_str(&format!("{key}: {}\n", serialize_yaml_value(value)));
    }

    fm.push_str("---\n\n");

    // Heading
    let mut body = format!("# {}\n\n", note.title);

    // Embed code (YouTube iframe)
    if let Some(embed) = &note.embed_code {
        body.push_str(embed);
        body.push_str("\n\n");
    }

    // Asset embed for file-based content
    match &note.content_type {
        ContentType::Image { asset_path } | ContentType::Pdf { asset_path } | ContentType::Document { asset_path } => {
            if let Some(filename) = std::path::Path::new(asset_path).file_name().and_then(|f| f.to_str()) {
                body.push_str(&format!("![[{filename}]]\n\n"));
            }
        }
        _ => {}
    }

    // Description callout (YouTube only)
    if let Some(ref desc) = note.description {
        body.push_str("> [!info]- Video Description\n");
        for line in desc.lines() {
            if line.trim().is_empty() {
                body.push_str(">\n");
            } else {
                body.push_str(&format!("> {line}\n"));
            }
        }
        body.push('\n');
    }

    // Why Captured (Phase 8): the operator's capture annotation, rendered
    // verbatim ABOVE `## Summary`. Emitted only when present and non-empty so a
    // bare capture renders no empty section.
    if let Some(capture) = note.capture_note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body.push_str("## Why Captured\n\n");
        body.push_str(capture);
        body.push_str("\n\n");
    }

    // Body: post-Phase-6 cutover prefers the pre-rendered structured body
    // produced by `distillers::render` (it already carries `## Summary` /
    // `## Claims` / `## Links` headings). The legacy `## Summary` wrapper
    // around `note.summary` is the fallback for non-URL kinds and for URL
    // kinds whose distillation produced no body (extreme fallback, never
    // expected in steady state because `fallback_distilled` always emits
    // a summary).
    if let Some(rendered) = &note.distilled_body
        && !rendered.trim().is_empty()
    {
        body.push_str(rendered);
        if !rendered.ends_with('\n') {
            body.push('\n');
        }
        if !rendered.ends_with("\n\n") {
            body.push('\n');
        }
    } else if !note.summary.is_empty() {
        body.push_str("## Summary\n\n");
        body.push_str(&note.summary);
        body.push_str("\n\n");
    }

    // Source footer
    if let Some(source) = &note.source_url {
        body.push_str(&format!("---\n\n*Source: [{source}]({source})*\n"));
    }

    format!("{fm}{body}")
}

/// Resolve the canonical `creator` for a note from its kind-specific data:
/// the YouTube uploader, the GitHub repo owner, or the article byline. Returns
/// `None` for kinds that carry no author and when the carried value is empty,
/// so the render can fall back to `default_creator`. Pure and testable.
pub fn creator_for(ct: &ContentType) -> Option<String> {
    let raw = match ct {
        ContentType::YouTube { uploader, .. } => Some(uploader.clone()),
        ContentType::GitHub { owner } => Some(owner.clone()),
        ContentType::Article { author } => author.clone(),
        _ => None,
    };
    raw.filter(|s| !s.trim().is_empty())
}

/// Render a string as a YAML scalar safe for inline insertion into the
/// hand-built frontmatter. Serializes through `serde_yaml` so backslashes,
/// embedded newlines, colons, and quotes are all quoted/escaped correctly.
/// The previous `escape_yaml_string` only escaped `"`, so a trailing `\` or
/// an embedded newline in any LLM-derived value (`title`, `creator`,
/// `cortex-repo-install`, ...) corrupted the entire frontmatter block
/// (empirically confirmed). The returned string includes whatever quoting
/// `serde_yaml` deems necessary and never carries a trailing newline.
fn yaml_scalar(s: &str) -> String {
    serde_yaml::to_string(&serde_yaml::Value::String(s.to_string()))
        .unwrap_or_else(|_| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .trim_end()
        .to_string()
}

/// Serialize a single `serde_yaml::Value` for inline insertion into the
/// hand-built frontmatter string. Scalars render bare; everything else
/// goes through `serde_yaml::to_string` and is reformatted to fit a
/// single key entry without disturbing the surrounding hand-built YAML.
fn serialize_yaml_value(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => yaml_scalar(s),
        serde_yaml::Value::Null => "null".to_string(),
        // Sequences and mappings: serialize, drop the leading newline that
        // `serde_yaml::to_string` emits for non-scalar values, and indent
        // each subsequent line with two spaces so the YAML stays valid
        // under the `key:` prefix.
        other => {
            let raw = serde_yaml::to_string(other).unwrap_or_default();
            let trimmed = raw.trim_end();
            let mut out = String::from("\n");
            for line in trimmed.lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
            out.pop();
            out
        }
    }
}

#[cfg(test)]
mod tests;
