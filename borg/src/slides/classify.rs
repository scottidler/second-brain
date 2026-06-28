//! Vision classification of slide frames (Phase 3 of content-aware filtering).
//!
//! One vision call per *run* (post `collapse_runs`) tags the run's most-complete
//! frame with a [`SlideClass`] - a taxonomy [`SlideCategory`] plus a confidence.
//! The orchestrator (Phase 4) keeps only runs whose category is in
//! `content-filter.keep` at or above `min-confidence`; everything else is
//! dropped. Classification is **fail-closed**: a malformed or ambiguous model
//! reply produces an `Err`, which drops that run rather than guessing a category.
//!
//! The fan-out is bounded by the **process-wide** vision permit pool in
//! `ocr` (sized by `content-filter.max-vision-concurrency`), so concurrent
//! videos plus the image-ingest path share one global Anthropic-call ceiling.
//!
//! See docs/design/2026-06-28-content-aware-slide-filtering.md.

use eyre::{Result, eyre};
use std::path::{Path, PathBuf};
use tokio::task::JoinSet;

use crate::config::{ContentFilterConfig, LlmConfig, SlideCategory, SlideClass, VisionConfig};
use crate::ocr;

/// Why a single classification did not yield a usable [`SlideClass`]. Carried as
/// a typed value (never re-parsed from an error string, per the typed-seam rule)
/// so the Phase 4 orchestrator can tally the *cause* of each drop. The two
/// failure causes are kept distinct because they mean different operational
/// things: an [`ClassifyError::Api`] (or [`ClassifyError::Read`]/[`ClassifyError::Join`])
/// is a degradation signal an operator must investigate, while a
/// [`ClassifyError::Parse`] is the model returning off-format text - a softer,
/// expected-occasionally failure. All variants are fail-closed: the run is dropped.
#[derive(Debug)]
pub enum ClassifyError {
    /// The frame file could not be read off disk.
    Read(eyre::Report),
    /// The vision API call itself failed (network, auth, 4xx/5xx, timeout).
    /// A nonzero count here is the operator's "filtering degraded" signal.
    Api(eyre::Report),
    /// The API returned text but it did not parse into a `CATEGORY`/`CONFIDENCE`.
    Parse(eyre::Report),
    /// The classification task panicked or was cancelled before producing a result.
    Join(eyre::Report),
}

impl std::fmt::Display for ClassifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassifyError::Read(e) => write!(f, "read frame: {e:#}"),
            ClassifyError::Api(e) => write!(f, "vision api: {e:#}"),
            ClassifyError::Parse(e) => write!(f, "parse reply: {e:#}"),
            ClassifyError::Join(e) => write!(f, "classification task: {e:#}"),
        }
    }
}

impl std::error::Error for ClassifyError {}

/// The closed-label taxonomy prompt sent with every classification call.
///
/// Three things this prompt must force, per the design doc:
/// 1. Classify by the **dominant content region**, ignoring small webcam insets
///    (picture-in-picture is the norm, not an edge case).
/// 2. Draw a **sharp line** between true diagrams
///    (`architecture-diagram`/`sequence-diagram`/`flowchart`) and
///    `infographic`/`chart`, which look diagram-ish but are not wired systems.
/// 3. Emit a **structured** `CATEGORY:`/`CONFIDENCE:` reply and nothing else, so
///    the parser is deterministic and can fail closed on anything off-format.
const TAXONOMY_PROMPT: &str = "\
You are classifying a single video frame ('slide') into exactly ONE category.

Classify by the DOMINANT content region - the largest, central thing on screen.
IGNORE small picture-in-picture webcam insets (a presenter's face in a corner):
a diagram with a webcam inset is a diagram; a webpage with a webcam inset is a webpage.

Categories (choose exactly one):
- architecture-diagram: components/systems wired together by arrows; data/control flow between boxes.
- sequence-diagram: UML or interaction sequence (lifelines, ordered messages between actors).
- flowchart: flow or decision diagram (process steps, decision diamonds, yes/no branches).
- code: source code shown on screen (an editor, a code block, syntax-highlighted text).
- terminal: a terminal, TUI, or CLI session / command output.
- infographic: a framework, maturity-model, quadrant, or marketing-style slide. NOT a real wired diagram - boxes that are LISTS or LABELS, not connected components.
- chart: a bar, line, scatter, pie, or plot chart of DATA.
- app-ui: an application GUI screenshot (buttons, menus, panels) that is not a webpage.
- webpage: a browser showing a blog, docs page, or website.
- talking-head: one or more people on camera with no dominant slide content.
- b-roll: physical objects, hardware, or scenery.
- title-card: a title, intro, section, or transition frame (large centered title text, little else).
- other: anything that fits none of the above.

Draw a SHARP line between true diagrams (architecture-diagram / sequence-diagram / flowchart),
which show connected components with flow, and infographic / chart, which present labels or data
without a wired system. When unsure between a diagram and an infographic, prefer infographic.

Respond with EXACTLY two lines and nothing else:
CATEGORY: <one of the category strings above, lowercase, hyphenated>
CONFIDENCE: <a number between 0.0 and 1.0>";

/// Classify a single slide frame on disk. Reads the JPEG bytes, derives its mime
/// from the extension, calls [`ocr::vision_classify`] with the taxonomy prompt,
/// and parses the structured reply.
///
/// **Fail-closed:** an unreadable file, an API error, or a malformed/ambiguous
/// model reply all return `Err`; the caller drops the run.
pub async fn classify_slide(
    frame: &Path,
    filter: &ContentFilterConfig,
    llm: &LlmConfig,
) -> std::result::Result<SlideClass, ClassifyError> {
    let bytes = std::fs::read(frame).map_err(|e| ClassifyError::Read(eyre!("read frame {}: {e}", frame.display())))?;
    let mime = ocr::mime_from_extension(&frame.to_string_lossy());

    log::debug!(
        "classify_slide: frame={} mime={mime} bytes={} model={}",
        frame.display(),
        bytes.len(),
        if filter.model.is_empty() { "<inherit>" } else { filter.model.as_str() },
    );

    // Build a one-off VisionConfig carrying the content-filter model override
    // (empty -> ocr falls back to llm.model). `enabled` is irrelevant here; the
    // caller already decided to classify.
    let vision = VisionConfig {
        enabled: true,
        model: filter.model.clone(),
    };

    let raw = ocr::vision_classify(&bytes, &mime, &vision, llm, TAXONOMY_PROMPT)
        .await
        .map_err(|e| ClassifyError::Api(eyre!("vision_classify {}: {e:#}", frame.display())))?;

    let class = parse_classification(&raw).map_err(|e| {
        ClassifyError::Parse(eyre!(
            "parse classification for {} (raw reply preview: {:?}): {e}",
            frame.display(),
            preview(&raw)
        ))
    })?;

    log::debug!(
        "classify_slide: frame={} category={:?} confidence={:.3}",
        frame.display(),
        class.category,
        class.confidence,
    );
    Ok(class)
}

/// Classify a batch of per-run best frames, one vision call per frame, returning
/// a result vector positionally aligned with `best_frames`. Each entry's `Err`
/// drops that single run downstream; the batch as a whole never fails.
///
/// The calls fan out across a [`JoinSet`]; actual in-flight Anthropic calls are
/// bounded by the process-wide vision permit pool inside `ocr::vision_classify`,
/// so this imposes no second per-video gate. Results are reordered back into the
/// input order via the spawned-task index so the vector aligns with
/// `best_frames`. A task panic (cancellation aside) is mapped to an `Err` so a
/// panicked classification still drops only its run, never the batch.
pub async fn classify_slides(
    best_frames: &[PathBuf],
    filter: &ContentFilterConfig,
    llm: &LlmConfig,
) -> Vec<std::result::Result<SlideClass, ClassifyError>> {
    log::debug!("classify_slides: runs={}", best_frames.len());

    type ClassifyResult = std::result::Result<SlideClass, ClassifyError>;
    let mut set: JoinSet<(usize, ClassifyResult)> = JoinSet::new();
    for (idx, frame) in best_frames.iter().enumerate() {
        let frame: PathBuf = frame.clone();
        let filter: ContentFilterConfig = filter.clone();
        let llm: LlmConfig = llm.clone();
        set.spawn(async move {
            log::trace!("classify_slides: dispatch idx={idx} frame={}", frame.display());
            (idx, classify_slide(&frame, &filter, &llm).await)
        });
    }

    let mut results: Vec<Option<ClassifyResult>> = (0..best_frames.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, res)) => results[idx] = Some(res),
            Err(e) => {
                // A join error (panic) has no index; record it against the first
                // still-empty slot so the count stays consistent and the run drops.
                log::warn!("classify_slides: classification task failed to join: {e}");
                if let Some(slot) = results.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(Err(ClassifyError::Join(eyre!("classification task panicked: {e}"))));
                }
            }
        }
    }

    let results: Vec<ClassifyResult> = results
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| Err(ClassifyError::Join(eyre!("classification result missing")))))
        .collect();

    let ok = results.iter().filter(|r| r.is_ok()).count();
    log::debug!(
        "classify_slides: classified={} ok={} err={}",
        results.len(),
        ok,
        results.len() - ok
    );
    results
}

/// The per-run keep/drop verdict, partitioning a classification result against
/// the `content-filter` policy. Each variant maps one-to-one to a tally bucket
/// in [`ClassifyTally`], so the orchestrator's observability line is derived
/// directly from these and can never drift from the actual decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepOutcome {
    /// Category is in `keep` and confidence is at/above `min-confidence` - embed.
    Keep,
    /// Confidence below `min-confidence` - dropped.
    DroppedLowConfidence,
    /// Category not listed in `keep` - dropped.
    DroppedNotInKeep,
    /// The vision API (or frame read / task join) failed - dropped, degradation signal.
    DroppedApiError,
    /// The API replied but the text was off-format - dropped.
    DroppedParseError,
}

/// Decide whether one classified run is kept, and if not, why. Pure: no I/O, no
/// network - the single source of truth for the keep-filter, unit-tested directly.
///
/// Order of checks matters for the tally: a failed classification is attributed
/// to its failure *cause* (api vs parse) before any category/confidence test, so
/// the degradation signal (`DroppedApiError`) is never masked as a policy drop.
pub fn keep_outcome(
    result: &std::result::Result<SlideClass, ClassifyError>,
    filter: &ContentFilterConfig,
) -> KeepOutcome {
    match result {
        Err(ClassifyError::Parse(_)) => KeepOutcome::DroppedParseError,
        // Read / Api / Join all mean "the classifier could not speak" - they are
        // the operator-facing degradation signal, distinct from the model
        // legitimately returning off-format text.
        Err(_) => KeepOutcome::DroppedApiError,
        Ok(class) => {
            if !filter.keep.contains(&class.category) {
                KeepOutcome::DroppedNotInKeep
            } else if class.confidence < filter.min_confidence {
                KeepOutcome::DroppedLowConfidence
            } else {
                KeepOutcome::Keep
            }
        }
    }
}

/// Per-ingest classification tally for the observability line. A nonzero
/// `dropped_api_error` is the operator's "filtering degraded" signal - distinct
/// from a legitimately image-free note (which classifies zero runs). Mirrors the
/// `degraded_24h` philosophy in `borg/AGENTS.md`: a silent-quality drop must be
/// counted, not hidden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassifyTally {
    /// Total runs classified (the input count to the keep-filter).
    pub classified: usize,
    /// Runs kept (category in `keep`, confidence at/above `min-confidence`).
    pub kept: usize,
    /// Dropped: confidence below `min-confidence`.
    pub dropped_low_confidence: usize,
    /// Dropped: category not in `keep`.
    pub dropped_not_in_keep: usize,
    /// Dropped: vision API / frame-read / task-join failure (degradation signal).
    pub dropped_api_error: usize,
    /// Dropped: model reply did not parse.
    pub dropped_parse_error: usize,
}

impl ClassifyTally {
    /// Record one run's verdict.
    pub fn record(&mut self, outcome: KeepOutcome) {
        self.classified += 1;
        match outcome {
            KeepOutcome::Keep => self.kept += 1,
            KeepOutcome::DroppedLowConfidence => self.dropped_low_confidence += 1,
            KeepOutcome::DroppedNotInKeep => self.dropped_not_in_keep += 1,
            KeepOutcome::DroppedApiError => self.dropped_api_error += 1,
            KeepOutcome::DroppedParseError => self.dropped_parse_error += 1,
        }
    }
}

/// Parse the structured `CATEGORY:`/`CONFIDENCE:` reply, fail-closed.
///
/// Returns `Err` when:
/// - either line is missing,
/// - the category string is not in the taxonomy (case-insensitively),
/// - the confidence is not a parseable number in `0.0..=1.0`,
/// - the category appears on more than one line with conflicting values (ambiguous).
///
/// Surrounding whitespace and an optional markdown bold/`-`/`*` prefix are
/// tolerated; anything genuinely off-format fails.
fn parse_classification(raw: &str) -> Result<SlideClass> {
    let mut category: Option<SlideCategory> = None;
    let mut category_seen_distinct = false;
    let mut confidence: Option<f32> = None;

    for line in raw.lines() {
        if let Some(rest) = strip_key(line, "CATEGORY") {
            let value = strip_value_decoration(rest);
            let parsed =
                SlideCategory::from_str_case_insensitive(value).ok_or_else(|| eyre!("unknown CATEGORY {value:?}"))?;
            match category {
                Some(prev) if prev != parsed => category_seen_distinct = true,
                _ => {}
            }
            category = Some(parsed);
        } else if let Some(rest) = strip_key(line, "CONFIDENCE") {
            let value = strip_value_decoration(rest);
            let parsed: f32 = value
                .parse()
                .map_err(|_| eyre!("CONFIDENCE {value:?} is not a number"))?;
            if !(0.0..=1.0).contains(&parsed) {
                return Err(eyre!("CONFIDENCE {parsed} out of range 0.0..=1.0"));
            }
            confidence = Some(parsed);
        }
    }

    if category_seen_distinct {
        return Err(eyre!("ambiguous reply: multiple conflicting CATEGORY lines"));
    }

    let category = category.ok_or_else(|| eyre!("reply missing CATEGORY line"))?;
    let confidence = confidence.ok_or_else(|| eyre!("reply missing CONFIDENCE line"))?;
    Ok(SlideClass { category, confidence })
}

/// Match `KEY:` (case-insensitive) on a line, returning everything after the
/// first colon. The key half tolerates a leading markdown list/bold decoration
/// (`-`, `*`, `**`, spaces) so `  - **CATEGORY:** code` still matches; the value
/// half is returned verbatim for the caller to clean via [`strip_value_decoration`].
fn strip_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (head, rest) = line.split_once(':')?;
    let head = head.trim().trim_matches(['*', '-', ' ']).trim();
    if head.eq_ignore_ascii_case(key) { Some(rest) } else { None }
}

/// Strip surrounding markdown bold decoration (`*`/`**`) and whitespace from a
/// VALUE, so `** code` -> `code` and `**0.75**` -> `0.75`. Crucially does NOT
/// strip a leading `-`: a `-0.1` confidence must survive intact so the numeric
/// range check rejects it (fail-closed), rather than being silently turned into
/// `0.1`.
fn strip_value_decoration(s: &str) -> &str {
    s.trim().trim_matches(['*', ' ']).trim()
}

/// A short, log-safe preview of a model reply (never inline the full payload).
fn preview(s: &str) -> String {
    const MAX: usize = 80;
    let one_line = s.replace('\n', " ");
    one_line.chars().take(MAX).collect()
}

#[cfg(test)]
mod tests;
