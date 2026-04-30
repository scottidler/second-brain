//! Stage 3: copy selected slides into the vault attachment area, emit
//! wikilink embeds in the note body, and produce the `slides:` frontmatter
//! list. See `docs/design/2026-04-29-frame-aware-youtube-ingestion.md`.

use chrono::{DateTime, Datelike, Utc};
use eyre::{Context, Result};
use std::path::Path;

use crate::slides::{NoteShape, SlideManifest, SummaryOutput, enforce_shape};

/// Outcome of publishing slides for one note.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// The final, possibly-downgraded note shape.
    pub shape: NoteShape,
    /// Vault-relative paths to slide JPEGs that the note owns.
    /// Empty for `text-only`. Goes verbatim into the note's `slides:` frontmatter.
    pub slides: Vec<String>,
    /// The note body to render under the frontmatter. For `text-only` this
    /// is the LLM's body verbatim. For `hero` / `slide-section` the body
    /// has wikilink embeds inserted at the right places.
    pub body: String,
}

/// Compute the per-month subdirectory for image attachments
/// (`images/YYYY-MM`), matching the existing convention.
pub fn month_subdir(now: &DateTime<Utc>) -> String {
    format!("images/{:04}-{:02}", now.year(), now.month())
}

/// Publish slides into the vault and return the final body + frontmatter slides list.
///
/// On `text-only`, no slides are copied and the body is the LLM's verbatim.
/// On `hero`, the first selected slide is copied and embedded once near the top.
/// On `slide-section`, every selected slide is copied and one wikilink is
/// inserted directly under the matching `## <section title>` heading in the
/// body.
///
/// File copies are atomic (write to `.tmp`, then rename). Slide IDs not
/// present in the manifest are dropped silently. Existing files at the
/// destination are NOT overwritten - on collision we pick a different
/// sequence suffix so a previously-published note's slides survive until
/// the cleanup step (Phase 2.2) deletes them.
pub fn publish_slides(
    vault_root: &Path,
    slug: &str,
    manifest: &SlideManifest,
    summary: &SummaryOutput,
    slides_source_root: &Path,
    now: &DateTime<Utc>,
) -> Result<PublishResult> {
    let (final_shape, final_slide_ids) =
        enforce_shape(manifest, &summary.frontmatter.shape, &summary.frontmatter.embed_slides);

    if matches!(final_shape, NoteShape::TextOnly) || final_slide_ids.is_empty() {
        return Ok(PublishResult {
            shape: NoteShape::TextOnly,
            slides: Vec::new(),
            body: summary.body.clone(),
        });
    }

    let subdir = month_subdir(now);
    let attachments_dir = vault_root.join("system/attachments").join(&subdir);
    std::fs::create_dir_all(&attachments_dir)
        .with_context(|| format!("create attachments dir: {}", attachments_dir.display()))?;

    let mut owned: Vec<String> = Vec::with_capacity(final_slide_ids.len());
    let mut filenames_for_ids: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for (i, slide_id) in final_slide_ids.iter().enumerate() {
        let slide = match manifest.slides.iter().find(|s| &s.id == slide_id) {
            Some(s) => s,
            None => continue,
        };
        // Source file path: when the manifest stores an absolute path use it
        // directly; otherwise resolve against the caller-supplied source root
        // (the staging / work directory the slides were materialized into).
        let src = if slide.frame_path.is_absolute() {
            slide.frame_path.clone()
        } else {
            slides_source_root.join(&slide.frame_path)
        };
        let basename = pick_filename(slug, i + 1, &attachments_dir);
        let dest = attachments_dir.join(&basename);
        atomic_copy(&src, &dest).with_context(|| format!("publish slide {} -> {}", src.display(), dest.display()))?;
        let vault_rel = format!("system/attachments/{subdir}/{basename}");
        owned.push(vault_rel.clone());
        filenames_for_ids.insert(slide_id.clone(), basename);
    }

    let body = match final_shape {
        NoteShape::TextOnly => summary.body.clone(),
        NoteShape::Hero => {
            // Insert exactly one wikilink at the top of the body.
            let first = filenames_for_ids
                .get(final_slide_ids.first().expect("hero requires one slide"))
                .map(String::as_str)
                .unwrap_or("");
            format!("![[{first}]]\n\n{body}", body = summary.body)
        }
        NoteShape::SlideSection => {
            insert_section_embeds(&summary.body, &summary.frontmatter.sections, &filenames_for_ids)
        }
    };

    Ok(PublishResult {
        shape: final_shape,
        slides: owned,
        body,
    })
}

/// Pick a filename `<slug>-slide-NNN.jpg` whose path does not yet exist in
/// `attachments_dir`. On collision (e.g. previous publish of the same slug
/// is being replaced and old files are still present) bump the sequence
/// number until a free slot is found. Caps at 999 iterations so we never
/// loop forever on a wedged directory.
fn pick_filename(slug: &str, requested: usize, attachments_dir: &Path) -> String {
    for n in requested..=requested.saturating_add(999) {
        let candidate = format!("{slug}-slide-{:03}.jpg", n);
        if !attachments_dir.join(&candidate).exists() {
            return candidate;
        }
    }
    // Fallback - this should never happen short of 1000 collisions.
    format!("{slug}-slide-{:03}.jpg", requested)
}

/// Atomic file copy: write bytes to `<dest>.tmp`, then rename. Crash mid-copy
/// leaves the dest absent or fully written, never half-written.
fn atomic_copy(src: &Path, dest: &Path) -> Result<()> {
    let tmp = dest.with_extension("tmp");
    let bytes = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

/// Insert a `![[<filename>]]` wikilink directly under each `## <title>`
/// heading whose title matches a `sections` entry's title. Sections without
/// a matching heading in the body are appended at the end (so the LLM's
/// section-title-versus-heading skew never silently drops a slide).
fn insert_section_embeds(
    body: &str,
    sections: &[crate::slides::SummarySection],
    filenames_for_ids: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(body.len() + sections.len() * 64);
    let mut placed: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
        if let Some(title) = line.strip_prefix("## ").map(str::trim_end)
            && let Some(section) = sections
                .iter()
                .find(|s| s.title.trim_end() == title && !placed.contains(s.slide.as_str()))
            && let Some(filename) = filenames_for_ids.get(&section.slide)
        {
            out.push_str(&format!("![[{filename}]]\n\n"));
            placed.insert(section.slide.as_str());
        }
    }

    // Append any unplaced sections at the end.
    for section in sections {
        if placed.contains(section.slide.as_str()) {
            continue;
        }
        if let Some(filename) = filenames_for_ids.get(&section.slide) {
            out.push_str(&format!("\n## {}\n\n![[{filename}]]\n", section.title));
        }
    }

    out
}

#[cfg(test)]
mod tests;
