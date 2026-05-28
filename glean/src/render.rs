//! Vault renderer for tier-2 distill output.
//!
//! Writes one markdown file per work-item under `notes/glean/`.
//! Identity is `content_hash`, NOT slug: on re-distill, the renderer
//! looks for an existing chunk with a matching `content-hash`
//! frontmatter and renames it in place when the slug shifted. The
//! fencepost-merge primitive (block.rs) is the only operator-edit
//! preservation seam.

pub mod block;

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};

use crate::types::{WorkItem, WorkItemKey};

const FENCEPOST_START: &str = "<!-- glean:fencepost-start -->";
const FENCEPOST_END: &str = "<!-- glean:fencepost-end -->";
const FRONTMATTER_FENCE: &str = "---";
const MAX_SLUG_LEN: usize = 80;

/// JSON shape the distill prompt produces.
#[derive(Debug, Clone)]
pub struct DistillOutput {
    pub title: String,
    pub tldr: String,
    pub setting: String,
    pub moves: Vec<String>,
    pub refusals: Vec<String>,
    pub carryover: String,
}

/// Write or update a chunk file for `work_item`. Returns the final
/// path of the chunk (which may differ from the slug-derived path
/// when the chunk already existed at a different slug and was
/// renamed in place).
///
/// Identity rule: scan `glean_dir` for any chunk whose frontmatter
/// `content-hash` matches `work_item.content_hash`. If found at a
/// different path, rename it to the new slug-derived path before
/// writing. If found at the current path, fencepost-merge. If not
/// found at all, write a new file.
pub fn render_chunk(
    glean_dir: &Path,
    work_item: &WorkItem,
    out: &DistillOutput,
    extractor: &str,
    extractor_model: &str,
) -> Result<PathBuf> {
    log::debug!(
        "render::render_chunk: content_hash={} title={}",
        &work_item.content_hash[..8.min(work_item.content_hash.len())],
        out.title
    );
    std::fs::create_dir_all(glean_dir).context("mkdir glean_dir")?;
    let base_slug = slugify(&out.title);
    let target_path = disambiguated_path(glean_dir, &base_slug, &work_item.content_hash)?;
    let existing = find_existing_by_content_hash(glean_dir, &work_item.content_hash)?;
    if let Some(existing_path) = existing
        && existing_path != target_path
    {
        log::info!(
            "render: renaming {} -> {} (slug churn; content-hash unchanged)",
            existing_path.display(),
            target_path.display()
        );
        std::fs::rename(&existing_path, &target_path)
            .with_context(|| format!("rename {} -> {}", existing_path.display(), target_path.display()))?;
    }

    let now = Utc::now();
    let body = compose_body(work_item, out, extractor, extractor_model, now);

    if target_path.exists() {
        let existing = std::fs::read_to_string(&target_path).context("read existing chunk")?;
        let merged = block::merge(&existing, &body);
        std::fs::write(&target_path, merged).context("write merged chunk")?;
    } else {
        std::fs::write(&target_path, body).context("write new chunk")?;
    }
    Ok(target_path)
}

fn compose_body(
    work_item: &WorkItem,
    out: &DistillOutput,
    extractor: &str,
    extractor_model: &str,
    now: DateTime<Utc>,
) -> String {
    let mut s = String::new();
    s.push_str(FRONTMATTER_FENCE);
    s.push('\n');
    s.push_str("type: glean\n");
    s.push_str(&format!("content-hash: {}\n", work_item.content_hash));
    s.push_str(&format!("work-item-key: {}\n", yaml_quote(&work_item.key_value)));
    s.push_str(&format!("work-item-key-type: {}\n", work_item.key_type.as_str()));
    if let Some(repo) = &work_item.repo_slug {
        s.push_str(&format!("repo: {}\n", yaml_quote(repo)));
    }
    s.push_str(&format!("title: {}\n", yaml_quote(&out.title)));
    s.push_str("sessions:\n");
    for u in &work_item.session_uuids {
        s.push_str(&format!("  - {}\n", yaml_quote(u)));
    }
    s.push_str(&format!("time-start: {}\n", work_item.time_start.to_rfc3339()));
    s.push_str(&format!("time-end: {}\n", work_item.time_end.to_rfc3339()));
    if !work_item.aggregated_tags.is_empty() {
        s.push_str("tags:\n");
        for t in &work_item.aggregated_tags {
            s.push_str(&format!("  - {}\n", yaml_quote(t)));
        }
    }
    s.push_str(&format!("extractor: {extractor}\n"));
    s.push_str(&format!("extractor-model: {extractor_model}\n"));
    s.push_str(&format!("extracted-at: {}\n", now.to_rfc3339()));
    s.push_str(FRONTMATTER_FENCE);
    s.push('\n');
    s.push('\n');
    s.push_str(&format!("# {}\n\n", out.title));
    s.push_str(&format!("> [!tldr]\n> {}\n\n", out.tldr.replace('\n', "\n> ")));
    s.push_str(FENCEPOST_START);
    s.push('\n');
    s.push_str("## Setting\n\n");
    s.push_str(out.setting.trim());
    s.push_str("\n\n## Moves\n\n");
    for m in &out.moves {
        let trimmed = m.trim();
        if trimmed.starts_with("- ") {
            s.push_str(trimmed);
        } else {
            s.push_str("- ");
            s.push_str(trimmed);
        }
        s.push('\n');
    }
    s.push_str("\n## Refusals\n\n");
    if out.refusals.is_empty() {
        s.push_str("- No load-bearing refusals in this work-item.\n");
    } else {
        for r in &out.refusals {
            let trimmed = r.trim();
            if trimmed.starts_with("- ") {
                s.push_str(trimmed);
            } else {
                s.push_str("- ");
                s.push_str(trimmed);
            }
            s.push('\n');
        }
    }
    s.push_str("\n## Carryover\n\n");
    s.push_str(out.carryover.trim());
    s.push_str("\n\n");
    s.push_str(FENCEPOST_END);
    s.push('\n');
    s
}

fn yaml_quote(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quote = s.contains(':')
        || s.contains('#')
        || s.contains('-')
        || s.contains('[')
        || s.contains('{')
        || s.contains('"')
        || s.contains('\'')
        || s.starts_with(' ')
        || s.ends_with(' ');
    if !needs_quote {
        return s.to_string();
    }
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Turn a title into a slug. Lowercased kebab-case, truncated to
/// `MAX_SLUG_LEN`. The `content_hash` is NOT mixed into the slug;
/// it lives in the frontmatter as the load-bearing identity. Slug
/// collisions are resolved by `disambiguated_slug` at render time.
pub fn slugify(title: &str) -> String {
    let kebab: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let mut squashed = String::with_capacity(kebab.len());
    let mut last_dash = false;
    for c in kebab.chars() {
        if c == '-' {
            if !last_dash {
                squashed.push('-');
            }
            last_dash = true;
        } else {
            squashed.push(c);
            last_dash = false;
        }
    }
    let trimmed = squashed.trim_matches('-').to_string();
    let stem: String = trimmed.chars().take(MAX_SLUG_LEN).collect();
    let stem = stem.trim_end_matches('-').to_string();
    if stem.is_empty() {
        return "glean-untitled".to_string();
    }
    stem
}

/// Find a non-colliding filename inside `glean_dir` starting from
/// `base_slug`. If `base_slug.md` is free or already belongs to this
/// `content_hash`, return that. Otherwise try `base_slug-2.md`,
/// `base_slug-3.md`, ... until a free name is found.
fn disambiguated_path(
    glean_dir: &Path,
    base_slug: &str,
    content_hash: &str,
) -> Result<PathBuf> {
    let primary = glean_dir.join(format!("{base_slug}.md"));
    if claim_path(&primary, content_hash)? {
        return Ok(primary);
    }
    for n in 2.. {
        let candidate = glean_dir.join(format!("{base_slug}-{n}.md"));
        if claim_path(&candidate, content_hash)? {
            return Ok(candidate);
        }
    }
    unreachable!("disambiguated_path: infinite loop bound by content-hash uniqueness")
}

fn claim_path(path: &Path, content_hash: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(true);
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(extract_frontmatter_content_hash(&raw).as_deref() == Some(content_hash))
}

/// Walk `glean_dir` looking for a chunk whose frontmatter has a
/// matching `content-hash`. Returns the first match (one is expected;
/// duplicates would imply two chunks with the same membership set and
/// represent a render-path bug elsewhere).
pub fn find_existing_by_content_hash(glean_dir: &Path, content_hash: &str) -> Result<Option<PathBuf>> {
    if !glean_dir.exists() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(glean_dir).context("read_dir glean_dir")? {
        let entry = entry.context("read_dir entry")?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        if let Some(hash) = extract_frontmatter_content_hash(&raw)
            && hash == content_hash
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn extract_frontmatter_content_hash(s: &str) -> Option<String> {
    let trimmed = s.trim_start();
    if !trimmed.starts_with(FRONTMATTER_FENCE) {
        return None;
    }
    let rest = &trimmed[FRONTMATTER_FENCE.len()..];
    let end = rest.find(FRONTMATTER_FENCE)?;
    let yaml_text = &rest[..end];
    for line in yaml_text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("content-hash:") {
            let v = rest.trim().trim_matches('"');
            return Some(v.to_string());
        }
    }
    None
}

/// The kebab name `work_item.key_value` resolves to for the
/// `notes/glean/` filesystem layout (without the `.md` extension).
/// Exposed for the CLI's `show` verb.
pub fn slug_for_work_item(work_item: &WorkItem) -> String {
    match work_item.key_type {
        WorkItemKey::DesignDoc => {
            let stem = Path::new(&work_item.key_value)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| work_item.key_value.clone());
            slugify(&stem)
        }
        WorkItemKey::Theme | WorkItemKey::Singleton => slugify(&work_item.key_value),
    }
}

#[cfg(test)]
mod tests;
