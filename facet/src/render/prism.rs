//! Prism renderer.
//!
//! One vault note per work-item, body composed of:
//!
//! ```text
//! ---
//! <frontmatter incl. facet-gem-count, facet-tag-mix>
//! ---
//!
//! # <work-item title>
//! ## Context
//! ## Gem Index   <-- list of "Gem N: <task>" with tag chips
//!
//! ## Gem 1: <task headline>
//!   ### Task
//!   ### Context
//!   ### Interaction
//!   ### Review
//! ## Gem 2: ...
//! ```
//!
//! Each section is wrapped in HTML-comment fenceposts so operator
//! content placed outside fenceposts survives re-renders. The same
//! `block::merge` machinery for fencepost-preserved operator content.
//!

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Context, Result};

use super::{block, write_atomic};
use crate::gems::{Gem, InteractionTurn};
use crate::workitem::WorkItem;

#[cfg(test)]
mod tests;

/// Render and write the prism note for `workitem`. Reads any
/// pre-existing file at `target_path` to preserve operator-owned
/// content. Writes atomically via tempfile + rename.
pub fn render_prism_note(target_path: &Path, workitem: &WorkItem, gems: &[Gem]) -> Result<()> {
    log::debug!(
        "render_prism_note: target={} workitem_id={} gems={}",
        target_path.display(),
        workitem.id,
        gems.len(),
    );
    let existing = if target_path.exists() {
        Some(
            std::fs::read_to_string(target_path)
                .with_context(|| format!("read existing prism note at {}", target_path.display()))?,
        )
    } else {
        None
    };
    let body = render_prism_to_string(workitem, gems, existing.as_deref());
    write_atomic(target_path, &body)
}

/// Pure render: build the merged note body. Public for golden tests.
pub fn render_prism_to_string(workitem: &WorkItem, gems: &[Gem], existing: Option<&str>) -> String {
    let fresh = build_fresh_template(workitem, gems);
    match existing {
        None => fresh,
        Some(existing) => block::merge(existing, &fresh),
    }
}

fn build_fresh_template(workitem: &WorkItem, gems: &[Gem]) -> String {
    let mut s = String::new();
    s.push_str("<!-- facet:auto:begin frontmatter -->\n");
    s.push_str(&render_frontmatter(workitem, gems));
    s.push_str("<!-- facet:auto:end frontmatter -->\n\n");

    s.push_str("<!-- facet:auto:begin header -->\n");
    s.push_str(&render_header(workitem, gems));
    s.push_str("<!-- facet:auto:end header -->\n\n");

    s.push_str("<!-- facet:auto:begin gem-index -->\n");
    s.push_str(&render_gem_index(gems));
    s.push_str("<!-- facet:auto:end gem-index -->\n\n");

    for (idx, gem) in gems.iter().enumerate() {
        let id = format!("gem:{}", gem.id);
        s.push_str(&format!("<!-- facet:auto:begin {id} -->\n"));
        s.push_str(&render_gem_section(idx + 1, gem));
        s.push_str(&format!("<!-- facet:auto:end {id} -->\n\n"));
    }

    s.push_str("<!-- facet:auto:begin footer -->\n");
    s.push_str(&render_footer(workitem));
    s.push_str("<!-- facet:auto:end footer -->\n");
    s
}

fn render_frontmatter(workitem: &WorkItem, gems: &[Gem]) -> String {
    let mut m = serde_yaml::Mapping::new();
    let insert_str = |m: &mut serde_yaml::Mapping, k: &str, v: String| {
        m.insert(serde_yaml::Value::String(k.into()), serde_yaml::Value::String(v));
    };
    insert_str(&mut m, "title", workitem.title.clone());
    insert_str(&mut m, "date", workitem.updated_at.format("%Y-%m-%d").to_string());
    insert_str(&mut m, "type", "facet-prism".to_string());
    insert_str(&mut m, "origin", "assisted".to_string());
    insert_str(&mut m, "method", "facet".to_string());
    insert_str(&mut m, "status", "unread".to_string());
    insert_str(&mut m, "domain", "ai".to_string());
    m.insert(
        serde_yaml::Value::String("facet-workitem-id".into()),
        serde_yaml::Value::Number(workitem.id.into()),
    );
    insert_str(&mut m, "facet-slug", workitem.slug.clone());
    insert_str(&mut m, "facet-status", workitem.status.as_str().to_string());
    m.insert(
        serde_yaml::Value::String("facet-sessions-count".into()),
        serde_yaml::Value::Number((workitem.sessions_count as u64).into()),
    );
    m.insert(
        serde_yaml::Value::String("facet-repos".into()),
        serde_yaml::Value::Sequence(
            workitem
                .repos
                .iter()
                .map(|r| serde_yaml::Value::String(r.clone()))
                .collect(),
        ),
    );
    m.insert(
        serde_yaml::Value::String("facet-gem-count".into()),
        serde_yaml::Value::Number((gems.len() as u64).into()),
    );
    m.insert(
        serde_yaml::Value::String("facet-tag-mix".into()),
        serde_yaml::Value::Sequence(
            tag_mix(gems)
                .into_iter()
                .map(|(tag, count)| {
                    let mut entry = serde_yaml::Mapping::new();
                    entry.insert(serde_yaml::Value::String("tag".into()), serde_yaml::Value::String(tag));
                    entry.insert(
                        serde_yaml::Value::String("count".into()),
                        serde_yaml::Value::Number((count as u64).into()),
                    );
                    serde_yaml::Value::Mapping(entry)
                })
                .collect(),
        ),
    );
    insert_str(
        &mut m,
        "facet-first-seen",
        workitem.created_at.format("%Y-%m-%d").to_string(),
    );
    insert_str(
        &mut m,
        "facet-last-seen",
        workitem.updated_at.format("%Y-%m-%d").to_string(),
    );
    m.insert(
        serde_yaml::Value::String("tags".into()),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("facet".into()),
            serde_yaml::Value::String("gem".into()),
        ]),
    );
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(m))
        .unwrap_or_else(|_| String::from("# yaml render failed\n"));
    format!("---\n{yaml}---\n")
}

fn render_header(workitem: &WorkItem, gems: &[Gem]) -> String {
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
        workitem.updated_at.format("%Y-%m-%d"),
    ));
    s.push_str(&format!("- **Status:** {}\n", workitem.status.as_str()));
    s.push_str(&format!("- **Gem count:** {}\n", gems.len()));
    let mix = tag_mix(gems);
    let mix_str: String = if mix.is_empty() {
        "*(none yet)*".to_string()
    } else {
        mix.iter()
            .map(|(t, n)| format!("`{t}`×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    s.push_str(&format!("- **Tag mix:** {mix_str}\n\n"));
    s
}

fn render_gem_index(gems: &[Gem]) -> String {
    let mut s = String::new();
    s.push_str("## Gem Index\n\n");
    if gems.is_empty() {
        s.push_str("*(no gems yet)*\n\n");
        return s;
    }
    for (idx, g) in gems.iter().enumerate() {
        let n = idx + 1;
        let tags = if g.tags.is_empty() {
            String::new()
        } else {
            format!(
                " — {}",
                g.tags.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(" ")
            )
        };
        s.push_str(&format!("{n}. [Gem {n}: {}](#gem-{n}){tags}\n", title_for(g)));
    }
    s.push('\n');
    s
}

fn render_gem_section(n: usize, gem: &Gem) -> String {
    let mut s = String::new();
    s.push_str(&format!("## Gem {n}: {}\n\n", title_for(gem)));
    s.push_str(&format!(
        "*Session `{}` · extracted {} · `{}` *\n\n",
        short_uuid(&gem.session_uuid),
        gem.extracted_at.format("%Y-%m-%dT%H:%M:%SZ"),
        gem.extractor_model,
    ));

    s.push_str("### Task\n\n");
    s.push_str(&format!("{}\n\n", escape_inline(&gem.task)));
    if !gem.why_it_matters.is_empty() {
        s.push_str(&format!("*Why it matters: {}*\n\n", escape_inline(&gem.why_it_matters)));
    }

    s.push_str("### Context\n\n");
    if gem.context_loaded.is_empty() {
        s.push_str("- *(none cited)*\n");
    } else {
        s.push_str("**Loaded by Scott:**\n\n");
        for item in &gem.context_loaded {
            s.push_str(&format!("- {}\n", escape_inline(item)));
        }
    }
    if !gem.context_missing.is_empty() {
        s.push_str("\n**Missing from AI:**\n\n");
        for item in &gem.context_missing {
            s.push_str(&format!("- {}\n", escape_inline(item)));
        }
    }
    s.push('\n');

    s.push_str("### Interaction\n\n");
    if gem.interaction.is_empty() {
        s.push_str("*(no turns captured)*\n\n");
    } else {
        for (i, turn) in gem.interaction.iter().enumerate() {
            s.push_str(&render_turn(i + 1, turn));
        }
    }

    s.push_str("### Review\n\n");
    let r = &gem.review;
    let mut any_review = false;
    if let Some(v) = &r.accepted {
        s.push_str(&format!("- **Accepted:** {}\n", escape_inline(v)));
        any_review = true;
    }
    if let Some(v) = &r.rejected {
        s.push_str(&format!("- **Rejected:** {}\n", escape_inline(v)));
        any_review = true;
    }
    if let Some(v) = &r.verified_manually {
        s.push_str(&format!("- **Verified manually:** {}\n", escape_inline(v)));
        any_review = true;
    }
    if let Some(v) = &r.rewrote_by_hand {
        s.push_str(&format!("- **Rewrote by hand:** {}\n", escape_inline(v)));
        any_review = true;
    }
    if !any_review {
        s.push_str("*(no review evidence in this slice)*\n");
    }
    s.push('\n');
    s
}

fn render_turn(seq: usize, turn: &InteractionTurn) -> String {
    let mut s = String::new();
    let tags_chip = if turn.tags.is_empty() {
        String::new()
    } else {
        format!(
            " — {}",
            turn.tags.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(" ")
        )
    };
    s.push_str(&format!("**Turn {seq}**{tags_chip}\n\n"));
    s.push_str("> **AI:**\n>\n");
    for line in turn.ai_says.lines() {
        s.push_str(&format!("> {line}\n"));
    }
    if turn.ai_says.is_empty() {
        s.push_str(">\n");
    }
    s.push_str(">\n");
    s.push_str("> **Scott:**\n>\n");
    for line in turn.user_says.lines() {
        s.push_str(&format!("> {line}\n"));
    }
    if turn.user_says.is_empty() {
        s.push_str(">\n");
    }
    s.push('\n');
    s
}

fn render_footer(workitem: &WorkItem) -> String {
    format!(
        "---\n\n*Rendered by `sb facet`. To re-render: `sb facet render {}`.*\n",
        workitem.slug,
    )
}

/// Gem-level tag counts, sorted by count descending then tag ascending.
fn tag_mix(gems: &[Gem]) -> Vec<(String, u32)> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for g in gems {
        for t in &g.tags {
            *counts.entry(t.clone()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, u32)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// First-sentence-ish headline derived from the gem's task field. Used
/// for the section heading. Truncates at 80 chars on a word boundary.
fn title_for(gem: &Gem) -> String {
    let raw = gem.task.trim();
    if raw.is_empty() {
        return "(untitled gem)".to_string();
    }
    let one_line = raw.split('\n').next().unwrap_or(raw).trim();
    if one_line.chars().count() <= 80 {
        return one_line.to_string();
    }
    let mut out = String::new();
    for word in one_line.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > 80 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.is_empty() {
        // Single mega-word — fall back to a char-bounded slice.
        out = one_line.chars().take(77).collect();
    }
    out.push('…');
    out
}

fn short_uuid(s: &str) -> &str {
    if s.len() <= 8 { s } else { &s[..8] }
}

fn escape_inline(s: &str) -> String {
    // Strip newlines and squeeze whitespace so the line stays well-formed
    // inside markdown bullets and emphasis spans. Heavy escaping is the
    // block renderer's job.
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        match ch {
            '\n' | '\r' | '\t' => {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            c if c.is_whitespace() => {
                if !prev_space {
                    out.push(' ');
                    prev_space = true;
                }
            }
            c => {
                out.push(c);
                prev_space = false;
            }
        }
    }
    out.trim().to_string()
}
