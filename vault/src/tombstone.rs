//! The soft-retire tombstone contract, owned here because TWO crates write
//! tombstones and neither may own the shape:
//!
//! - `cortex::association` retires a note absorbed by a similarity merge.
//! - `borg::dedupe` retires the surplus forks a single harvest trace produced
//!   (design `2026-08-15-harvest-note-identity-trace-keyed-replace.md`, Phase 6).
//!
//! Before this module the two carried independent hardcoded copies of
//! `"superseded-by"` and `"Merged into [[{stem}]].\n"`, each pinned only by its
//! own crate-local test - so changing one crate and its test left the other
//! green while the vault silently grew two tombstone dialects. Readers of
//! tombstones (`cortex::association`'s skip guard, `borg::dedupe`'s grouping and
//! `--purge` link check, `borg::harvest::identity`'s tombstone follower) key on
//! this shape, so a dialect split is a correctness bug, not a cosmetic one.
//!
//! Schema-is-law: the strings live here once, and both writers import them.
//!
//! NOTE: the two writers still differ in HOW they rewrite the frontmatter -
//! cortex does text-level field surgery (preserving byte formatting of untouched
//! keys), borg parses to `Frontmatter` and re-serializes (normalizing key order).
//! That difference is deliberate and out of scope for this contract, which fixes
//! the KEY NAMES and the BODY TEXT - the parts every reader depends on.

/// Frontmatter key marking a note as retired, holding the survivor's filename
/// STEM (not a path - Obsidian resolves `[[stem]]` vault-wide, which is what
/// lets a tombstone keep resolving after cortex moves either note).
pub const SUPERSEDED_BY_KEY: &str = "superseded-by";

/// Frontmatter key stripped from a tombstone so it can never re-group.
pub const SLUG_KEY: &str = "slug";

/// The tombstone's entire body: a single redirect wikilink to the survivor.
///
/// This is why neither writer needs to rewrite inbound links: every inbound
/// `[[link]]` (piped, path-qualified, embedded, or in a `.base` file) still
/// resolves to the tombstone, which redirects here.
pub fn redirect_body(survivor_stem: &str) -> String {
    format!("Merged into [[{survivor_stem}]].\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_body_is_a_single_wikilink_line() {
        assert_eq!(redirect_body("some-survivor"), "Merged into [[some-survivor]].\n");
    }

    /// Pins the contract itself. Both `cortex::association::tombstone_content`
    /// and `borg::dedupe::apply_group` build their output from these values, so
    /// changing one here changes both writers at once - which is the point.
    /// If you are here because this failed, you are changing the tombstone
    /// dialect for every already-retired note in the vault.
    #[test]
    fn the_tombstone_contract_is_pinned() {
        assert_eq!(SUPERSEDED_BY_KEY, "superseded-by");
        assert_eq!(SLUG_KEY, "slug");
        assert_eq!(redirect_body("x"), "Merged into [[x]].\n");
    }
}
