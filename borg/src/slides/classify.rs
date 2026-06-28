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

use eyre::{Context, Result, eyre};
use std::path::{Path, PathBuf};
use tokio::task::JoinSet;

use crate::config::{ContentFilterConfig, LlmConfig, SlideCategory, SlideClass, VisionConfig};
use crate::ocr;

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
pub async fn classify_slide(frame: &Path, filter: &ContentFilterConfig, llm: &LlmConfig) -> Result<SlideClass> {
    let bytes = std::fs::read(frame).with_context(|| format!("read frame {}", frame.display()))?;
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
        .with_context(|| format!("vision_classify {}", frame.display()))?;

    let class = parse_classification(&raw).with_context(|| {
        format!(
            "parse classification for {} (raw reply preview: {:?})",
            frame.display(),
            preview(&raw)
        )
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
) -> Vec<Result<SlideClass>> {
    log::debug!("classify_slides: runs={}", best_frames.len());

    let mut set: JoinSet<(usize, Result<SlideClass>)> = JoinSet::new();
    for (idx, frame) in best_frames.iter().enumerate() {
        let frame: PathBuf = frame.clone();
        let filter: ContentFilterConfig = filter.clone();
        let llm: LlmConfig = llm.clone();
        set.spawn(async move {
            log::trace!("classify_slides: dispatch idx={idx} frame={}", frame.display());
            (idx, classify_slide(&frame, &filter, &llm).await)
        });
    }

    let mut results: Vec<Option<Result<SlideClass>>> = (0..best_frames.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((idx, res)) => results[idx] = Some(res),
            Err(e) => {
                // A join error (panic) has no index; record it against the first
                // still-empty slot so the count stays consistent and the run drops.
                log::warn!("classify_slides: classification task failed to join: {e}");
                if let Some(slot) = results.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(Err(eyre!("classification task panicked: {e}")));
                }
            }
        }
    }

    let results: Vec<Result<SlideClass>> = results
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| Err(eyre!("classification result missing"))))
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
