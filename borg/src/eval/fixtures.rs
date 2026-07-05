//! Golden distillation fixtures: `(source, distilled)` pairs loaded from
//! `config/eval/distill-fixtures/<kind>/<slug>/{source.md,distilled.yml}`.
//!
//! The kind is the first path component; the slug is the second. `distilled.yml`
//! deserializes into the canonical [`vault::distilled::Distilled`] contract, so
//! the harness scores exactly the artifact the pipeline produces.

use std::path::Path;

use eyre::{Context, Result, bail};
use vault::distilled::Distilled;

/// The source file inside each fixture directory (verbatim source text).
const SOURCE_FILE: &str = "source.md";
/// The distilled artifact inside each fixture directory.
const DISTILLED_FILE: &str = "distilled.yml";

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
            fixtures.push(Fixture {
                id: format!("{kind}/{slug}"),
                kind: kind.clone(),
                slug,
                source,
                distilled,
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
/// the summary followed by each claim (text + trailing `[anchor]` when present).
/// This is the same shape the judge would see in a published note's body, minus
/// vault-specific decoration.
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
    }
    out
}

#[cfg(test)]
mod tests;
