//! Fencepost-merging vault note renderer.
//!
//! The renderer is a structured merge, not an overwrite. Managed
//! sections are wrapped in HTML-comment fenceposts:
//!
//! ```text
//! <!-- facet:auto:begin section:frame -->
//! ...generated content...
//! <!-- facet:auto:end section:frame -->
//! ```
//!
//! Content OUTSIDE fenceposts is operator-owned and preserved across
//! re-renders. Frontmatter is treated as a single `Auto { id:
//! "frontmatter" }` block; `facet-*` keys are facet-owned, all other
//! keys are operator-extensible (the renderer reads, merges, writes).

pub mod block;
pub mod frontmatter;
pub mod prism;
pub mod quarantine;

use std::path::{Path, PathBuf};

use eyre::{Context, Result};

use crate::extract::JudgmentMoment;
use crate::workitem::WorkItem;

/// Fixed scaffolding-mode order; section appears even when empty (the
/// fencepost stays present so operator content placed near it does not
/// move on a regeneration).
const SCAFFOLD_MODES: &[(&str, &str)] = &[
    ("frame", "Frame"),
    ("iterate", "Iterate"),
    ("reject", "Reject"),
    ("push-for", "Push for"),
    ("sequence", "Sequence"),
    ("name-the-failure", "Name the failure"),
];

/// Render and write the work-item note for `workitem`. Reads any
/// pre-existing file at `target_path` to preserve operator-owned
/// content. Writes atomically via tempfile + rename.
pub fn render_work_item_note(target_path: &Path, workitem: &WorkItem, moments: &[JudgmentMoment]) -> Result<()> {
    log::debug!(
        "render_work_item_note: target={} workitem_id={} moments={}",
        target_path.display(),
        workitem.id,
        moments.len()
    );
    let existing = if target_path.exists() {
        Some(
            std::fs::read_to_string(target_path)
                .with_context(|| format!("read existing work-item note at {}", target_path.display()))?,
        )
    } else {
        None
    };
    let body = render_to_string(workitem, moments, existing.as_deref());
    write_atomic(target_path, &body)
}

/// Pure render: build the merged note body. Public for golden tests.
pub fn render_to_string(workitem: &WorkItem, moments: &[JudgmentMoment], existing: Option<&str>) -> String {
    let fresh_template = build_fresh_template(workitem, moments, existing);
    match existing {
        None => fresh_template,
        Some(existing) => block::merge(existing, &fresh_template),
    }
}

fn build_fresh_template(workitem: &WorkItem, moments: &[JudgmentMoment], existing: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str("<!-- facet:auto:begin frontmatter -->\n");
    s.push_str(&frontmatter::render(workitem, moments, existing));
    s.push_str("<!-- facet:auto:end frontmatter -->\n\n");

    s.push_str("<!-- facet:auto:begin header -->\n");
    s.push_str(&render_header(workitem, moments));
    s.push_str("<!-- facet:auto:end header -->\n\n");

    for (mode, label) in SCAFFOLD_MODES {
        let id = format!("section:{mode}");
        s.push_str(&format!("<!-- facet:auto:begin {id} -->\n"));
        s.push_str(&render_section(label, mode, moments));
        s.push_str(&format!("<!-- facet:auto:end {id} -->\n\n"));
    }

    s.push_str("<!-- facet:auto:begin section:other -->\n");
    s.push_str(&render_other_section(moments));
    s.push_str("<!-- facet:auto:end section:other -->\n\n");

    s.push_str("<!-- facet:auto:begin footer -->\n");
    s.push_str(&render_footer(workitem));
    s.push_str("<!-- facet:auto:end footer -->\n");
    s
}

fn render_header(workitem: &WorkItem, moments: &[JudgmentMoment]) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", workitem.title));
    s.push_str("## Context\n\n");
    s.push_str(&format!(
        "- **Repos:** {}\n",
        if workitem.repos.is_empty() {
            "*(none)*".to_string()
        } else {
            workitem.repos.join(", ")
        }
    ));
    s.push_str(&format!(
        "- **Sessions:** {}, first {}, last {}\n",
        workitem.sessions_count,
        workitem.created_at.format("%Y-%m-%d"),
        workitem.updated_at.format("%Y-%m-%d")
    ));
    s.push_str(&format!("- **Status:** {}\n", workitem.status.as_str()));
    let modes: Vec<String> = collected_modes(moments);
    s.push_str(&format!(
        "- **Judgment modes present:** {}\n",
        if modes.is_empty() { "*(none yet)*".to_string() } else { modes.join(", ") }
    ));
    s
}

fn render_section(label: &str, mode: &str, moments: &[JudgmentMoment]) -> String {
    let mut s = format!("## {label}\n\n");
    let matching: Vec<&JudgmentMoment> = moments.iter().filter(|m| m.mode == mode).collect();
    if matching.is_empty() {
        s.push_str("*(no moments yet)*\n\n");
        return s;
    }
    for m in matching {
        s.push_str(&render_moment_block(m));
    }
    s
}

fn render_moment_block(m: &JudgmentMoment) -> String {
    let mut s = String::new();
    s.push_str(&format!("> {}\n>\n", escape_inline(&m.ai_move)));
    s.push_str("> ```text\n");
    for line in m.quote_excerpt.lines() {
        s.push_str(&format!("> {line}\n"));
    }
    if !m.quote_excerpt.ends_with('\n') && m.quote_excerpt.is_empty() {
        // empty quote: emit a blank line inside the fence so the markdown is well-formed
        s.push_str(">\n");
    }
    s.push_str("> ```\n");
    s.push_str(&format!(
        ">\n> *Why it matters: {}*\n",
        escape_inline(&m.why_it_matters)
    ));
    s.push_str(&format!(
        ">\n> - `{}` at `{}`\n\n",
        short_uuid(&m.session_uuid),
        m.extracted_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    s
}

fn render_other_section(moments: &[JudgmentMoment]) -> String {
    let mut s = String::new();
    let scaffold: std::collections::HashSet<&str> = SCAFFOLD_MODES.iter().map(|(m, _)| *m).collect();
    let mut other_modes: Vec<&str> = moments
        .iter()
        .map(|m| m.mode.as_str())
        .filter(|m| !scaffold.contains(m))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    other_modes.sort();
    if other_modes.is_empty() {
        return s;
    }
    for mode in other_modes {
        let label = humanise_mode(mode);
        s.push_str(&render_section(&label, mode, moments));
    }
    s
}

fn render_footer(workitem: &WorkItem) -> String {
    let mut s = String::new();
    s.push_str("---\n\n");
    s.push_str(&format!(
        "*This note was synthesized by `sb facet`. To re-render: `sb facet render {}`.*\n",
        workitem.slug
    ));
    s
}

fn collected_modes(moments: &[JudgmentMoment]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in moments {
        set.insert(m.mode.clone());
    }
    set.into_iter().collect()
}

fn humanise_mode(mode: &str) -> String {
    let mut s = String::new();
    for (i, segment) in mode.split('-').enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let mut chars = segment.chars();
        if let Some(c) = chars.next() {
            s.push(c.to_ascii_uppercase());
            s.push_str(chars.as_str());
        }
    }
    s
}

fn short_uuid(s: &str) -> &str {
    s.get(..8).unwrap_or(s)
}

fn escape_inline(s: &str) -> String {
    s.replace('\n', " ").trim().to_string()
}

pub(crate) fn write_atomic(path: &Path, body: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("target path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    let tmp = make_temp_path(path);
    std::fs::write(&tmp, body).with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn make_temp_path(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let base = target.file_name().and_then(|s| s.to_str()).unwrap_or("facet.tmp");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".{base}.tmp-{pid}-{nanos}"))
}

#[cfg(test)]
mod tests;
