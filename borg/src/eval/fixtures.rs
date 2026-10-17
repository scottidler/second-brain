//! Golden distillation fixtures: `(source, distilled)` pairs loaded from
//! `config/eval/distill-fixtures/<kind>/<slug>/{source.md,distilled.yml}`,
//! plus an optional `capture-note.txt` (the reason the source was saved, the
//! capture note `sb borg eval --regenerate` distills with).
//!
//! The kind is the first path component; the slug is the second. `distilled.yml`
//! deserializes into the canonical [`vault::distilled::Distilled`] contract, so
//! the harness scores exactly the artifact the pipeline produces.
//!
//! [`load_staged`] builds the same `Fixture` shape from borg's live staging
//! root instead, so the eval can score what the pipeline publishes today.

use std::collections::BTreeMap;
use std::path::Path;

use distillers::RenderOptions;
use eyre::{Context, Result, bail};
use vault::distilled::Distilled;

/// The source file inside each fixture directory (verbatim source text).
const SOURCE_FILE: &str = "source.md";
/// The distilled artifact inside each fixture directory.
const DISTILLED_FILE: &str = "distilled.yml";
/// The optional capture note inside each fixture directory, written by
/// `sb borg eval --synth-intents`.
pub const CAPTURE_NOTE_FILE: &str = "capture-note.txt";

/// One golden fixture: a source and the distilled artifact scored against it.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// `<kind>/<slug>`, the stable identity used in the judgment cache key.
    pub id: String,
    /// Content kind (`article`, `video`, `thread`, `repo`, `image`,
    /// `voicenote`, `idea`), taken from the parent directory name.
    pub kind: String,
    /// Fixture directory name.
    pub slug: String,
    /// Verbatim source text the distiller received.
    pub source: String,
    /// The distilled artifact being scored.
    pub distilled: Distilled,
    /// The fixture's `capture-note.txt`, trimmed; `None` when the file is
    /// absent or blank (a blank note is a bare capture, as on ingest).
    pub capture_note: Option<String>,
}

/// Load every fixture under `dir`, sorted by id for deterministic output.
///
/// A directory is a fixture iff it contains BOTH `source.md` and
/// `distilled.yml`. Kind directories with neither (e.g. a stray `README.md`
/// file) are skipped. An empty tree is an error — the harness has nothing to
/// measure.
pub fn load(dir: &Path) -> Result<Vec<Fixture>> {
    log::debug!("eval::fixtures::load: dir={}", dir.display());
    if !dir.is_dir() {
        bail!("distillation fixtures dir not found: {}", dir.display());
    }

    let mut fixtures = Vec::new();
    for kind_entry in read_sorted(dir)? {
        let kind_path = kind_entry.path();
        if !kind_path.is_dir() {
            continue;
        }
        let kind = kind_entry.file_name().to_string_lossy().to_string();
        for slug_entry in read_sorted(&kind_path)? {
            let slug_path = slug_entry.path();
            if !slug_path.is_dir() {
                continue;
            }
            let source_path = slug_path.join(SOURCE_FILE);
            let distilled_path = slug_path.join(DISTILLED_FILE);
            if !source_path.is_file() || !distilled_path.is_file() {
                continue;
            }
            let slug = slug_entry.file_name().to_string_lossy().to_string();
            let source =
                std::fs::read_to_string(&source_path).with_context(|| format!("reading {}", source_path.display()))?;
            let raw = std::fs::read_to_string(&distilled_path)
                .with_context(|| format!("reading {}", distilled_path.display()))?;
            let distilled: Distilled =
                serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", distilled_path.display()))?;
            let capture_note = read_capture_note(&slug_path)?;
            fixtures.push(Fixture {
                id: format!("{kind}/{slug}"),
                kind: kind.clone(),
                slug,
                source,
                distilled,
                capture_note,
            });
        }
    }

    fixtures.sort_by(|a, b| a.id.cmp(&b.id));
    if fixtures.is_empty() {
        bail!("no distillation fixtures found under {}", dir.display());
    }
    log::debug!("eval::fixtures::load: loaded {} fixtures", fixtures.len());
    Ok(fixtures)
}

/// The trimmed `capture-note.txt` of a fixture directory, `None` when absent or
/// blank. A present file that cannot be read is an error.
fn read_capture_note(fixture_dir: &Path) -> Result<Option<String>> {
    let path = fixture_dir.join(CAPTURE_NOTE_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// Per-trace files [`load_staged`] reads.
const ENVELOPE_FILE: &str = "envelope.yml";
const TRANSCRIPT_FILE: &str = "transcript.md";
const FETCHED_FILE: &str = "fetched.html";
const BODY_FILE: &str = "body.txt";

#[derive(serde::Deserialize)]
struct StagedEnvelope {
    kind: crate::types::IngestKind,
}

/// Map a staged ingest kind onto the fixture kind labels (the golden tree's
/// directory names), so staged and golden runs aggregate under one vocabulary.
/// `None` for kinds the eval does not score (vocabulary cards).
fn fixture_kind(kind: crate::types::IngestKind) -> Option<&'static str> {
    use crate::types::IngestKind as K;
    match kind {
        K::ArticleUrl => Some("article"),
        K::GitHubUrl => Some("repo"),
        K::YoutubeUrl => Some("video"),
        K::ThreadUrl => Some("thread"),
        K::Image => Some("image"),
        K::VoiceNote => Some("voicenote"),
        K::Idea => Some("idea"),
        K::Session => Some("session"),
        K::VocabularyEn | K::VocabularyEs => None,
    }
}

/// The source text a staged trace's distiller saw, best evidence first: the
/// `transcript` the distiller persisted, the Stage-1 `transcript.md`, the
/// README decoded from a GitHub Stage-0 envelope, the fetched page, the raw
/// body. A body that is only the captured URL is not a source.
fn staged_source(trace_dir: &Path, distilled: &Distilled) -> Result<Option<String>> {
    if let Some(t) = distilled.transcript.as_deref().filter(|t| !t.trim().is_empty()) {
        return Ok(Some(t.to_string()));
    }
    let fetched = trace_dir.join(FETCHED_FILE);
    if !trace_dir.join(TRANSCRIPT_FILE).is_file() && fetched.is_file() {
        let bytes = std::fs::read(&fetched).with_context(|| format!("reading {}", fetched.display()))?;
        if bytes.first() == Some(&b'{')
            && let Ok(readme) = crate::github::readme_from_stage0(&bytes)
        {
            return Ok(Some(readme));
        }
    }
    for name in [TRANSCRIPT_FILE, FETCHED_FILE, BODY_FILE] {
        let path = trace_dir.join(name);
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let trimmed = text.trim();
        if trimmed.is_empty() || (!trimmed.contains(char::is_whitespace) && trimmed.starts_with("http")) {
            continue;
        }
        return Ok(Some(text));
    }
    Ok(None)
}

/// What one staging directory yielded.
enum StagedLoad {
    Loaded(Box<Fixture>),
    /// A staged trace the eval does not score: an unscored kind, or no
    /// recoverable source.
    Skipped,
    /// Not a staged distillation at all (no `envelope.yml` / `distilled.yml`).
    NotStaged,
}

fn load_staged_dir(dir: &Path, trace: &str) -> Result<StagedLoad> {
    let distilled_path = dir.join(DISTILLED_FILE);
    let envelope_path = dir.join(ENVELOPE_FILE);
    if !distilled_path.is_file() || !envelope_path.is_file() {
        return Ok(StagedLoad::NotStaged);
    }
    let envelope_raw =
        std::fs::read_to_string(&envelope_path).with_context(|| format!("reading {}", envelope_path.display()))?;
    let envelope: StagedEnvelope =
        serde_yaml::from_str(&envelope_raw).with_context(|| format!("parsing {}", envelope_path.display()))?;
    let Some(kind) = fixture_kind(envelope.kind) else {
        return Ok(StagedLoad::Skipped);
    };
    let raw =
        std::fs::read_to_string(&distilled_path).with_context(|| format!("reading {}", distilled_path.display()))?;
    let distilled: Distilled =
        serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", distilled_path.display()))?;
    let Some(source) = staged_source(dir, &distilled)? else {
        log::debug!("eval::fixtures::load_staged: no source for trace={trace}");
        return Ok(StagedLoad::Skipped);
    };
    Ok(StagedLoad::Loaded(Box::new(Fixture {
        id: format!("{kind}/{trace}"),
        kind: kind.to_string(),
        slug: trace.to_string(),
        source,
        distilled,
        capture_note: None,
    })))
}

/// Load ONE staged trace by id: `Ok(None)` when it is absent, is not a scored
/// kind, or has no recoverable source (swept from staging, say); `Err` when
/// its artifacts exist but do not parse. An id that is not a bare directory
/// name never reaches the filesystem.
pub fn load_staged_trace(root: &Path, trace: &str) -> Result<Option<Fixture>> {
    log::debug!(
        "eval::fixtures::load_staged_trace: root={} trace={trace}",
        root.display()
    );
    let bare = !trace.is_empty() && !trace.contains(['/', '\\']) && trace != "." && trace != "..";
    if !bare {
        return Ok(None);
    }
    match load_staged_dir(&root.join(trace), trace)? {
        StagedLoad::Loaded(fixture) => Ok(Some(*fixture)),
        StagedLoad::Skipped | StagedLoad::NotStaged => Ok(None),
    }
}

/// What one staged trace yielded, for a reader that must tell a swept trace
/// from a broken one ([`probe_staged_trace`]).
#[derive(Debug)]
pub enum StagedTrace {
    /// The trace loaded, with its source.
    Loaded(Box<Fixture>),
    /// No staging directory for the trace: swept by retention (or never staged).
    Absent,
    /// The directory exists but yields no scorable source: missing artifacts,
    /// an unscored kind, no recoverable source, or a parse error. Carries why.
    Unreadable(String),
}

/// Load ONE staged trace by id, classifying every way it can fail to load.
/// Shares [`load_staged_trace`]'s reader; unlike it, an absent directory and an
/// unreadable one are different answers, and a parse error is a value, not an
/// `Err`. An id that is not a bare directory name is `Unreadable` and never
/// reaches the filesystem.
pub fn probe_staged_trace(root: &Path, trace: &str) -> StagedTrace {
    log::debug!(
        "eval::fixtures::probe_staged_trace: root={} trace={trace}",
        root.display()
    );
    let bare = !trace.is_empty() && !trace.contains(['/', '\\']) && trace != "." && trace != "..";
    if !bare {
        return StagedTrace::Unreadable(format!("trace id {trace:?} is not a bare directory name"));
    }
    let dir = root.join(trace);
    if !dir.is_dir() {
        return StagedTrace::Absent;
    }
    match load_staged_dir(&dir, trace) {
        Ok(StagedLoad::Loaded(fixture)) => StagedTrace::Loaded(fixture),
        Ok(StagedLoad::NotStaged) => {
            StagedTrace::Unreadable(format!("{} lacks {ENVELOPE_FILE} or {DISTILLED_FILE}", dir.display()))
        }
        Ok(StagedLoad::Skipped) => StagedTrace::Unreadable(format!(
            "{} is an unscored kind or has no recoverable source",
            dir.display()
        )),
        Err(e) => StagedTrace::Unreadable(format!("{e:#}")),
    }
}

/// Load live distillations from borg's staging root as fixtures: every trace
/// directory holding `envelope.yml` + `distilled.yml` + a recoverable source.
/// Ids are `<kind>/<trace>`. With `sample`, take a deterministic stratified
/// sample: round-robin across kinds, each kind ordered by a stable hash of its
/// trace id, so reruns pick the same traces and every kind is represented.
pub fn load_staged(root: &Path, sample: Option<usize>) -> Result<Vec<Fixture>> {
    log::debug!(
        "eval::fixtures::load_staged: root={} sample={:?}",
        root.display(),
        sample
    );
    if !root.is_dir() {
        bail!("staging root not found: {}", root.display());
    }
    let mut loaded = Vec::new();
    let mut skipped = 0usize;
    for entry in read_sorted(root)? {
        let dir = entry.path();
        let trace = entry.file_name().to_string_lossy().to_string();
        match load_staged_dir(&dir, &trace)? {
            StagedLoad::Loaded(fixture) => loaded.push(*fixture),
            StagedLoad::Skipped => skipped += 1,
            StagedLoad::NotStaged => {}
        }
    }
    let total = loaded.len();
    let picked = stratified_sample(loaded, sample);
    if picked.is_empty() {
        bail!("no staged distillations found under {}", root.display());
    }
    log::debug!(
        "eval::fixtures::load_staged: loaded={} of {} skipped={}",
        picked.len(),
        total,
        skipped
    );
    Ok(picked)
}

/// A deterministic stratified sample of `fixtures`: round-robin across kinds,
/// each kind ordered by a stable hash of its trace id, so reruns pick the same
/// traces and every kind is represented. `None` keeps them all. Sorted by id.
pub fn stratified_sample(fixtures: Vec<Fixture>, sample: Option<usize>) -> Vec<Fixture> {
    let mut by_kind: BTreeMap<String, Vec<Fixture>> = BTreeMap::new();
    for fixture in fixtures {
        by_kind.entry(fixture.kind.clone()).or_default().push(fixture);
    }
    for fixtures in by_kind.values_mut() {
        fixtures.sort_by_key(|f| crate::eval::cache::stable_hash(&f.slug));
    }
    let total: usize = by_kind.values().map(Vec::len).sum();
    let take = sample.unwrap_or(total).min(total);
    let mut queues: Vec<std::vec::IntoIter<Fixture>> = by_kind.into_values().map(Vec::into_iter).collect();
    let mut picked = Vec::with_capacity(take);
    while picked.len() < take {
        for q in queues.iter_mut() {
            if picked.len() == take {
                break;
            }
            if let Some(f) = q.next() {
                picked.push(f);
            }
        }
    }
    picked.sort_by(|a, b| a.id.cmp(&b.id));
    picked
}

/// Directory entries sorted by file name (deterministic traversal).
fn read_sorted(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .with_context(|| format!("reading dir {}", dir.display()))?
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("iterating dir {}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// Render the distilled artifact into the plain-text "note" the judge scores:
/// the summary, each claim (text + trailing `[anchor]` when present, then its
/// `who:` and `quote:` lines when present), then the tools the note renders
/// (`distillers::tools_to_render`: name, URL, command). This is the same
/// content a published note's body carries, minus vault-specific decoration.
pub fn judge_note_text(distilled: &Distilled) -> String {
    let mut out = String::new();
    out.push_str("SUMMARY:\n");
    out.push_str(distilled.summary.trim());
    out.push_str("\n\nCLAIMS:\n");
    for claim in &distilled.claims {
        out.push_str("- ");
        out.push_str(claim.text.trim());
        if let Some(anchor) = &claim.anchor {
            let anchor = anchor.trim();
            if !anchor.is_empty() {
                out.push_str(&format!(" [{anchor}]"));
            }
        }
        out.push('\n');
        if let Some(who) = claim.who.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            out.push_str(&format!("  who: {who}\n"));
        }
        if let Some(quote) = claim.quote.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            out.push_str(&format!("  quote: \"{quote}\"\n"));
        }
    }
    let tools = distillers::tools_to_render(distilled);
    if !tools.is_empty() {
        out.push_str("\nTOOLS:\n");
        for tool in &tools {
            out.push_str("- ");
            out.push_str(tool.name.trim());
            if let Some(url) = tool.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
                out.push(' ');
                out.push_str(url);
            }
            if let Some(command) = tool.command.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                out.push_str(": ");
                out.push_str(command);
            }
            out.push('\n');
        }
    }
    out
}

/// The `RenderOptions` a real publish/backfill call site would apply to a
/// fixture of this `kind` (distillation-output-restore design:
/// `sb borg eval` note-size wiring). Video/Article/Repo/Thread all render at
/// borg's single URL site (`RenderOptions::for_url_publish`, keyed on the
/// typed `KindPayload` so only Thread keeps its transcript); the
/// verbatim-preservation kinds (Image/VoiceNote/Idea/Vocabulary) render at
/// their own always-`true` seams in `text.rs`/`handlers.rs`. A fixture only
/// carries the kind label (its directory name), so this maps that label to
/// the same per-kind policy production uses - never a fixture-specific guess,
/// and never a blanket `include_transcript: false` that would understate a
/// verbatim kind's true published size.
pub fn render_options_for_kind(kind: &str, distilled: &Distilled) -> RenderOptions {
    match kind {
        "video" | "article" | "repo" | "thread" => RenderOptions::for_url_publish(distilled),
        // Session notes publish transcript-free (the verbatim transcript is a
        // consumed staging intermediate, never in the note body), so the eval
        // note-size must exclude it too - matching the harvest publish path.
        "session" => RenderOptions {
            include_transcript: false,
        },
        _ => RenderOptions {
            include_transcript: true,
        },
    }
}

#[cfg(test)]
mod tests;
