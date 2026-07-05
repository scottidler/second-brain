//! Render a `Distilled` payload into the markdown body + frontmatter
//! additions that Stage 3 (publish) writes to the vault file.
//!
//! Pure functions: no I/O, no SQLite. The vault file is the canonical store;
//! `index_vault` later parses these sections back into the FTS5 index.

use std::collections::BTreeMap;
use vault::distilled::{Claim, ClaimKind, Distilled, KindPayload};

/// Output of the renderer. The caller (Stage 3) is responsible for splicing
/// `body_markdown` into the published note's body and merging
/// `frontmatter_additions` into the published note's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedDistilled {
    /// Markdown sections (`## Summary`, `## Claims`, `## Links`) ready to be
    /// inserted into the note body. Always ends with a trailing newline.
    pub body_markdown: String,
    /// Frontmatter keys produced by the Distilled. Includes the `distilled`
    /// control flag, the extractor id, and per-kind `cortex-*` metadata.
    /// Insertion order is alphabetical (BTreeMap) for stable diffs.
    ///
    /// DELIBERATE EXCEPTION to the `cortex-*` naming contract: the top-level
    /// `github:` key (a YAML sequence of `owner/repo` slugs harvested from a
    /// video description) is emitted bare, NOT as `cortex-github`. It is a
    /// first-class vault field consumed by Dataview/queries as `github`, not a
    /// cortex-managed metadata field. This is the one sanctioned non-`cortex-*`
    /// addition; everything else stays prefixed.
    pub frontmatter_additions: BTreeMap<String, serde_yaml::Value>,
}

/// Render a Distilled into the body + frontmatter additions Stage 3 writes
/// to the vault file. Pure function (no I/O); the file system writer is the
/// caller's responsibility.
pub fn render(distilled: &Distilled) -> RenderedDistilled {
    let mut body = String::new();
    push_summary(&mut body, &distilled.summary);
    push_claims(&mut body, &distilled.claims);
    push_links(&mut body, &distilled.links);
    push_transcript(&mut body, distilled.transcript.as_deref());

    let mut fm: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    fm.insert("distilled".to_string(), serde_yaml::Value::Bool(true));
    fm.insert(
        "distilled-extractor".to_string(),
        serde_yaml::Value::String(distilled.meta.extractor.clone()),
    );

    match &distilled.kind_specific {
        Some(KindPayload::Repo(p)) => {
            if let Some(stars) = p.stars {
                fm.insert("cortex-repo-stars".to_string(), serde_yaml::Value::Number(stars.into()));
            }
            if let Some(lang) = &p.primary_language {
                fm.insert(
                    "cortex-repo-primary-language".to_string(),
                    serde_yaml::Value::String(lang.clone()),
                );
            }
            if let Some(commit) = &p.last_commit {
                fm.insert(
                    "cortex-repo-last-commit".to_string(),
                    serde_yaml::Value::String(commit.clone()),
                );
            }
            if !p.topics.is_empty() {
                fm.insert(
                    "cortex-repo-topics".to_string(),
                    serde_yaml::Value::Sequence(p.topics.iter().cloned().map(serde_yaml::Value::String).collect()),
                );
            }
            if let Some(install) = &p.install
                && install.chars().count() <= 500
            {
                fm.insert(
                    "cortex-repo-install".to_string(),
                    serde_yaml::Value::String(install.clone()),
                );
            }
        }
        Some(KindPayload::Video(p)) => {
            if let Some(channel) = &p.channel {
                fm.insert(
                    "cortex-video-channel".to_string(),
                    serde_yaml::Value::String(channel.clone()),
                );
            }
            if let Some(duration) = p.duration_seconds {
                fm.insert(
                    "cortex-video-duration-seconds".to_string(),
                    serde_yaml::Value::Number(duration.into()),
                );
            }
            if let Some(published) = &p.published_at {
                fm.insert(
                    "cortex-video-published-at".to_string(),
                    serde_yaml::Value::String(published.clone()),
                );
            }
            if !p.repos.is_empty() {
                fm.insert(
                    "github".to_string(),
                    serde_yaml::Value::Sequence(p.repos.iter().cloned().map(serde_yaml::Value::String).collect()),
                );
            }
        }
        Some(KindPayload::Thread(p)) => {
            fm.insert(
                "cortex-thread-platform".to_string(),
                serde_yaml::Value::String(p.platform.clone()),
            );
            fm.insert(
                "cortex-thread-post-count".to_string(),
                serde_yaml::Value::Number(p.post_count.into()),
            );
            if let Some(author) = &p.author {
                fm.insert(
                    "cortex-thread-author".to_string(),
                    serde_yaml::Value::String(author.clone()),
                );
            }
        }
        None => {}
    }

    RenderedDistilled {
        body_markdown: body,
        frontmatter_additions: fm,
    }
}

fn push_summary(body: &mut String, summary: &str) {
    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return;
    }
    body.push_str("## Summary\n\n");
    // Fallback summaries lead with `[reason]` followed by a raw transcript
    // snippet that can open with a markdown heading (`#` / `##`); demote any
    // embedded headings so they stay subordinate to this `## Summary` L2
    // heading rather than colliding with the note's section structure.
    body.push_str(&crate::text::demote_headings(trimmed, 2));
    body.push_str("\n\n");
}

fn push_claims(body: &mut String, claims: &[Claim]) {
    if claims.is_empty() {
        return;
    }
    body.push_str("## Claims\n\n");
    for claim in claims {
        body.push_str("- ");
        // Decoration prefix: `**kind**` (omitted for `fact`, the default, so a
        // legacy fact claim with no who/quote renders byte-identically to the
        // pre-Phase-3 shape) and `(who)` (omitted when absent), joined by a
        // space and terminated by `: ` before the claim text.
        let kind_part = (claim.kind != ClaimKind::Fact).then(|| format!("**{}**", claim.kind.as_str()));
        let who_part = claim
            .who
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| format!("({w})"));
        let prefix = match (kind_part, who_part) {
            (Some(k), Some(w)) => format!("{k} {w}"),
            (Some(k), None) => k,
            (None, Some(w)) => w,
            (None, None) => String::new(),
        };
        if !prefix.is_empty() {
            body.push_str(&prefix);
            body.push_str(": ");
        }
        // Flatten any internal line breaks: a multi-line claim would otherwise
        // break the `- ` bullet into stray continuation lines.
        body.push_str(&crate::text::flatten_lines(claim.text.trim()));
        if let Some(anchor) = &claim.anchor
            && !anchor.is_empty()
        {
            body.push_str(" [");
            body.push_str(anchor);
            body.push(']');
        }
        body.push('\n');
        // Optional verbatim quote as an indented blockquote line beneath the
        // bullet. Flattened so a multi-line quote can't spill into stray lines.
        if let Some(quote) = &claim.quote
            && !quote.trim().is_empty()
        {
            body.push_str("  > \"");
            body.push_str(&crate::text::flatten_lines(quote.trim()));
            body.push_str("\"\n");
        }
    }
    body.push('\n');
}

fn push_transcript(body: &mut String, transcript: Option<&str>) {
    let Some(text) = transcript else {
        return;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    body.push_str("## Transcript\n\n");
    // Defense-in-depth: even if a caller passes pre-formatted markdown as
    // the transcript (cortex backfill demotes upstream already, but other
    // callers might not), demote any H1/H2 inside so the embedded headings
    // can't collide with the surrounding L2 section structure.
    body.push_str(&crate::text::demote_headings(trimmed, 2));
    body.push_str("\n\n");
}

fn push_links(body: &mut String, links: &[vault::distilled::Link]) {
    if links.is_empty() {
        return;
    }
    body.push_str("## Links\n\n");
    for link in links {
        body.push_str("- ");
        if let Some(label) = &link.label
            && !label.is_empty()
        {
            body.push('[');
            body.push_str(label);
            body.push_str("](");
            body.push_str(&link.url);
            body.push(')');
        } else {
            body.push_str(&link.url);
        }
        body.push('\n');
    }
    body.push('\n');
}

#[cfg(test)]
mod tests;
