//! Presentation rendering: reformat a narrative into a slide-deck
//! outline (one slide per gem citation, speaker notes inline).
//!
//! Phase 7. Output is markdown with `---` slide separators, suitable
//! for paste into Slides/Keynote/markdown-slides converters.

use crate::narrative::Narrative;

#[cfg(test)]
mod tests;

/// Render a narrative as a slide-deck outline.
pub fn render_outline(narrative: &Narrative) -> String {
    let mut s = String::new();
    // Title slide.
    s.push_str(&format!("# {}\n\n", narrative.title));
    s.push_str(&format!("> {}\n\n", narrative.thesis));
    s.push_str(&format!(
        "*Synthesised {} · model {} · revision {}*\n\n",
        narrative.synthesised_at.format("%Y-%m-%d"),
        narrative.synthesiser_model,
        narrative.revision,
    ));
    s.push_str("---\n\n");

    // Setup / thesis slide. Pull the first chunk of body_md if present.
    s.push_str("## Setup\n\n");
    let body_summary = body_first_paragraph(&narrative.body_md);
    s.push_str(&body_summary);
    s.push_str("\n\n");
    s.push_str("---\n\n");

    // One slide per gem citation.
    for (idx, gem_id) in narrative.gem_ids.iter().enumerate() {
        s.push_str(&format!("## Beat {}: gem #{gem_id}\n\n", idx + 1));
        s.push_str(&format!(
            "*Speaker notes: see canonical prism note for full dialog of gem #{gem_id}.*\n\n",
        ));
        s.push_str("---\n\n");
    }

    // Closing slide.
    s.push_str("## Takeaway\n\n");
    s.push_str(&format!(
        "{} → cluster of {} gem(s) tells one story.\n\n",
        narrative.thesis,
        narrative.gem_ids.len()
    ));
    s.push_str("---\n\n");
    s.push_str(&format!("*Rendered by `sb facet present {}`.*\n", narrative.slug));
    s
}

fn body_first_paragraph(body_md: &str) -> String {
    let mut buf = String::new();
    for line in body_md.lines() {
        if line.trim().is_empty() && !buf.is_empty() {
            break;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
    }
    if buf.is_empty() { "*(thesis above)*".to_string() } else { buf }
}
