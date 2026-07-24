//! `cortex associate`: groups harvest session notes that share a
//! content-derived `slug:` (borg's deterministic collision naming, shipped
//! v0.12.2) and, per pairwise similarity, decides whether to merge them into
//! one note or cross-link them (2026-07-24 cortex-association-sweep design).
//!
//! Phase 1 lands the pure grouping core plus the shared config/opts shapes.
//! The similarity decision core (transitive clustering), merge executor,
//! cross-link executor, and CLI/daemon wiring are later phases of the same
//! design - see `docs/design/2026-07-24-cortex-association-sweep.md`.

use std::collections::BTreeMap;

use vault::schema::NoteType;

use crate::vault::Note;

/// Group session notes that share the same content-derived `slug:`
/// frontmatter value (borg's harvest naming, v0.12.2 - a slug collision is
/// an association signal, not a naming accident, per the design's Problem
/// Statement).
///
/// Scoped to `content_type == Session`: this action never associates
/// non-session notes (that is cross-slug work, `cortex::duplicates`' job).
/// Skips notes with `slug == None` (legacy pre-slug notes; a separate
/// harvest-slug migration re-slugs them, out of scope here) and notes
/// carrying a `superseded-by:` tombstone - an already-absorbed note must
/// never re-group, which is what makes the future merge executor's
/// soft-retire idempotent.
///
/// Groups with fewer than two members are dropped: a lone note has nothing
/// to associate with.
///
/// Returned as index groups into the input `notes` slice (not cloned
/// `Note`s) so the caller controls ownership. BTreeMap-ordered by slug so
/// the group order - and therefore any downstream deterministic tie-break -
/// never depends on `notes`' scan order or hash-map iteration order.
pub fn group_by_slug(notes: &[Note]) -> Vec<Vec<usize>> {
    log::debug!("association::group_by_slug: notes={}", notes.len());
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, note) in notes.iter().enumerate() {
        if note.frontmatter.note_type.as_deref() != Some(NoteType::Session.as_str()) {
            continue;
        }
        if note.frontmatter.extra.contains_key("superseded-by") {
            continue;
        }
        let Some(slug) = note.frontmatter.extra.get("slug").and_then(|v| v.as_str()) else {
            continue;
        };
        groups.entry(slug.to_string()).or_default().push(i);
    }
    let result: Vec<Vec<usize>> = groups.into_values().filter(|members| members.len() >= 2).collect();
    log::debug!(
        "association::group_by_slug: groups={} (singletons, legacy, and tombstoned notes dropped)",
        result.len()
    );
    result
}

#[cfg(test)]
mod tests;
