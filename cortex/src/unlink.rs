//! Retraction sweep: strip stoplisted wikilink markup the auto-linker
//! already landed in note bodies.
//!
//! The stopword gate in `crate::linking` stops the NEXT false link; the
//! graph stopword stops the edge. Neither retracts markup that already
//! landed - `graph::tests::stoplisted_wikilink_leaves_the_note_body_byte_identical`
//! pins that as the graph phase's binding rule. This module is the one
//! explicit, operator-invoked phase that edits the landed bytes, which is
//! why it is a separate verb (`sb cortex unlink`) and reports before it
//! writes rather than running on the daemon tick.
//!
//! Scope is deliberately the same as the linker's WRITER scope: authored
//! notes and hub bodies are skipped, because the linker never wrote there,
//! so any `[[every]]` in them is somebody else's - Scott's prose or the hub
//! builder's verbatim render. Retracting only what the linker could have
//! written keeps the sweep a true inverse rather than a vault-wide edit.

use eyre::Result;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::stopwords::Stopwords;
use crate::vault::Note;

/// A wikilink with its raw target and optional display text. The target
/// class excludes `#`/`^` so heading and block refs (`[[note#heading]]`)
/// never match a bare stopword and never get rewritten.
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]|#^]+)(?:\|([^\[\]]+))?\]\]").expect("valid wikilink regex"));

/// One note's retractions for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlinkChange {
    pub path: PathBuf,
    /// The raw `[[target]]` that was stoplisted, in the first spelling seen
    /// in this note. Occurrences are grouped case-insensitively, matching how
    /// the stopword itself matches.
    pub target: String,
    pub occurrences: usize,
}

/// Outcome of a sweep. `applied == false` is a dry run: `changes` is fully
/// populated and nothing was written.
#[derive(Debug, Clone, Default)]
pub struct UnlinkStats {
    pub applied: bool,
    pub scanned: usize,
    pub skipped_authored: usize,
    pub skipped_hub: usize,
    /// Notes whose bytes changed (or would change, on a dry run).
    pub files_changed: usize,
    /// Total `[[target]]` occurrences retracted (or that would be).
    pub occurrences: usize,
    pub changes: Vec<UnlinkChange>,
}

/// Strip every stoplisted wikilink from `notes`, writing only when `apply`.
///
/// `include_authored` widens the scope to `origin: authored` notes; see
/// `crate::opts::UnlinkOpts` for why that is opt-in and when it is right.
///
/// Returns typed data; printing is the caller's job (`sb/src/cli/cortex.rs`).
pub fn run_with_notes(
    vault_root: &Path,
    notes: &[Note],
    stopwords: &Stopwords,
    apply: bool,
    include_authored: bool,
) -> Result<UnlinkStats> {
    log::debug!(
        "unlink::run_with_notes: vault_root={} notes={} stopwords={} apply={} include_authored={}",
        vault_root.display(),
        notes.len(),
        stopwords.len(),
        apply,
        include_authored
    );

    let mut stats = UnlinkStats {
        applied: apply,
        ..Default::default()
    };

    // An empty vocabulary is a no-op, not a full-vault rewrite. Returning
    // early also keeps the log honest about why nothing happened.
    if stopwords.is_empty() {
        log::info!("unlink: no wikilink stopwords configured; nothing to retract");
        return Ok(stats);
    }

    for note in notes {
        stats.scanned += 1;

        // Same exclusions as `linking::lint_linking`'s writer path.
        if !include_authored && crate::scope::is_authored(note) {
            stats.skipped_authored += 1;
            continue;
        }
        if note.path.starts_with(crate::hub::HUB_DIR) {
            stats.skipped_hub += 1;
            continue;
        }

        // Split on the RAW file so the frontmatter prefix is preserved byte
        // for byte: `Note::body` is trimmed, so reassembling from it would
        // silently eat the blank line after the closing `---`.
        let (prefix_len, body) = match ::vault::frontmatter::split_raw(&note.raw) {
            Some((_, body)) => (note.raw.len() - body.len(), body),
            None => (0, note.raw.as_str()),
        };

        let (new_body, hits) = retract(body, stopwords);
        if hits.is_empty() {
            continue;
        }

        stats.files_changed += 1;
        for (target, occurrences) in hits {
            stats.occurrences += occurrences;
            stats.changes.push(UnlinkChange {
                path: note.path.clone(),
                target,
                occurrences,
            });
        }

        if apply {
            let abs_path = vault_root.join(&note.path);
            let new_raw = format!("{}{}", &note.raw[..prefix_len], new_body);
            ::vault::note::write_atomic(&abs_path, new_raw.as_bytes())?;
            log::info!("unlink: retracted stoplisted wikilinks in {}", note.path.display());
        }
    }

    log::info!(
        "unlink complete: {} occurrence(s) across {} file(s) (applied={})",
        stats.occurrences,
        stats.files_changed,
        apply
    );
    Ok(stats)
}

/// Rewrite `body`, replacing every stoplisted `[[target]]` / `[[target|display]]`
/// with the text a reader already sees. Returns the new body plus per-target
/// occurrence counts in first-seen order.
///
/// Idempotent by construction: the output contains no stoplisted wikilink, so
/// a second pass finds nothing.
fn retract(body: &str, stopwords: &Stopwords) -> (String, Vec<(String, usize)>) {
    let mut out = String::with_capacity(body.len());
    let mut hits: Vec<(String, usize)> = Vec::new();
    let mut last = 0;

    for cap in LINK_RE.captures_iter(body) {
        let whole = cap.get(0).expect("group 0 always matches");
        let target = cap.get(1).expect("target group is not optional").as_str();

        if !stopwords.contains(target) {
            continue;
        }
        // `![[embed]]` transcludes a note; the auto-linker never writes one,
        // and unwrapping it would change what renders, not just how it links.
        if body[..whole.start()].ends_with('!') {
            log::trace!("unlink: leaving transclusion ![[{target}]] alone");
            continue;
        }
        // The linker refuses to write into code, so anything here is source
        // material a reader is meant to see verbatim.
        if crate::linking::in_code_context(body, whole.start()) {
            log::trace!("unlink: leaving [[{target}]] inside code alone");
            continue;
        }

        // Piped links keep their display text; a bare link falls back to the
        // target as written, so `[[Every]]` retracts to `Every` (case intact).
        let replacement = match cap.get(2) {
            Some(display) => display.as_str(),
            None => target.trim(),
        };

        out.push_str(&body[last..whole.start()]);
        out.push_str(replacement);
        last = whole.end();

        // Group the way the stopword MATCHES - case-insensitively - or a note
        // carrying both `[[Every]]` and `[[every|Every]]` reports two rows for
        // what is one stoplisted target. The first spelling seen is the label.
        let key = target.trim();
        match hits.iter_mut().find(|(t, _)| t.eq_ignore_ascii_case(key)) {
            Some((_, count)) => *count += 1,
            None => hits.push((key.to_string(), 1)),
        }
    }

    out.push_str(&body[last..]);
    (out, hits)
}

#[cfg(test)]
mod tests;
