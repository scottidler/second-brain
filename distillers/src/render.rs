//! Render a `Distilled` payload into the markdown body + frontmatter
//! additions that Stage 3 (publish) writes to the vault file.
//!
//! Pure functions: no I/O, no SQLite. The vault file is the canonical store;
//! `index_vault` later parses these sections back into the FTS5 index.

use std::collections::BTreeMap;
use vault::distilled::{Claim, ClaimKind, Distilled, Enumeration, KindPayload, SessionPayload, Tool};

/// Caller policy for [`render`]. Transcript emission is the caller's decision,
/// not a global kind rule (distillation-output-restore): borg
/// publish suppresses `## Transcript` for Video/Article/Repo (the staged
/// `distilled.yml` keeps the verbatim text and cortex embeds it from staging),
/// while the verbatim-preservation kinds (VoiceNote/Idea/Vocabulary/Image/
/// Thread) and cortex `summarize --backfill` still emit it. The choice is
/// carried as this typed value at the render seam — never inferred from an
/// extractor string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Emit the `## Transcript` body section when `Distilled.transcript` is
    /// `Some`. `false` for Video/Article/Repo publish; `true` for the verbatim
    /// kinds and cortex summarize backfill (which re-renders the legacy note
    /// body from the transcript field and would destroy it otherwise).
    pub include_transcript: bool,
}

impl RenderOptions {
    /// Transcript policy for borg's single URL render site (Video, Article,
    /// Repo, and Thread all render there). Only Thread — a
    /// verbatim-preservation kind — keeps its in-note `## Transcript`;
    /// Video/Article/Repo publish transcript-free (the verbatim text lives in
    /// the staged `distilled.yml`, embedded from there). Keyed on the typed
    /// `KindPayload`, never on an extractor string. Note that at this site an
    /// absent payload (`None`) is the Article case, which is correctly `false`;
    /// the other transcript-keeping kinds (Idea/Vocabulary/Image/VoiceNote) do
    /// NOT pass through this site — they have their own always-`true` seams.
    pub fn for_url_publish(distilled: &Distilled) -> Self {
        Self {
            include_transcript: matches!(distilled.kind_specific, Some(KindPayload::Thread(_))),
        }
    }
}

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
/// caller's responsibility. `options` carries the caller's transcript-emission
/// policy (see [`RenderOptions`]).
///
/// Body section order (April `obsidian-note.md` shape restored): the `> [!tldr]`
/// callout, `## Summary`, `## Enumerated Points` (listicles only), `## Key
/// Ideas` (omitted when empty), `## Claims`, `## Tools & Commands` (omitted when
/// there is nothing to show), `## Links`, then `## Transcript`
/// last (only when `options.include_transcript`). `## Why Captured`, when
/// present, is prepended by the borg markdown layer above this body.
pub fn render(distilled: &Distilled, options: RenderOptions) -> RenderedDistilled {
    let mut body = String::new();
    push_tldr(&mut body, distilled.tldr.as_deref());
    push_summary(&mut body, &distilled.summary);
    push_enumeration(&mut body, distilled.enumeration.as_ref());
    push_key_ideas(&mut body, &distilled.key_ideas);
    push_claims(&mut body, &distilled.claims);
    let tools = tools_to_render(distilled);
    push_tools(&mut body, &tools);
    push_links(&mut body, &distilled.links, &tools);
    // Session body footer (harvest-clyde-sessions): list the member
    // sessions as `clyde://` back-pointers so `clyde session resume` recovers
    // full fidelity (the conductor-validated back-pointer, vault-native).
    if let Some(KindPayload::Session(p)) = &distilled.kind_specific {
        push_session_footer(&mut body, p);
    }
    if options.include_transcript {
        push_transcript(&mut body, distilled.transcript.as_deref());
    }

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
            for (key, value) in [
                ("channel-id", &p.channel_id),
                ("channel-handle", &p.channel_handle),
                ("channel-url", &p.channel_url),
            ] {
                if let Some(v) = value {
                    fm.insert(key.to_string(), serde_yaml::Value::String(v.clone()));
                }
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
        // Bare schema seam (harvest-clyde-sessions); the
        // session-specific body footer (member sessions, thread membership)
        // is the `render()` extension, not this frontmatter tagging arm.
        Some(KindPayload::Session(p)) => {
            if p.msg_count > 0 {
                fm.insert(
                    "cortex-session-msg-count".to_string(),
                    serde_yaml::Value::Number(p.msg_count.into()),
                );
            }
            if !p.session_ids.is_empty() {
                fm.insert(
                    "cortex-session-ids".to_string(),
                    serde_yaml::Value::Sequence(p.session_ids.iter().cloned().map(serde_yaml::Value::String).collect()),
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
    body.push_str(&crate::text::literal_wikilinks(&crate::text::demote_headings(
        trimmed, 2,
    )));
    body.push_str("\n\n");
}

/// One line of model-written prose: flattened, with any `[[...]]` the model
/// emitted kept literal (see `text::literal_wikilinks`).
fn prose_line(text: &str) -> String {
    crate::text::literal_wikilinks(&crate::text::flatten_lines(text))
}

fn push_tldr(body: &mut String, tldr: Option<&str>) {
    let Some(text) = tldr else {
        return;
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    // Obsidian `[!tldr]` callout. `cortex::quality` keys on the literal
    // `> [!tldr]` marker as a summary signal. Flatten so a multi-sentence hook
    // can't break the callout syntax into stray blockquote continuation lines.
    body.push_str("> [!tldr]\n> ");
    body.push_str(&prose_line(trimmed));
    body.push_str("\n\n");
}

fn push_enumeration(body: &mut String, enumeration: Option<&Enumeration>) {
    let Some(enumeration) = enumeration else {
        return;
    };
    if enumeration.items.is_empty() {
        return;
    }
    body.push_str("## Enumerated Points\n\n");
    if let Some(lead_in) = &enumeration.lead_in {
        let trimmed = lead_in.trim();
        if !trimmed.is_empty() {
            body.push_str(&prose_line(trimmed));
            body.push_str("\n\n");
        }
    }
    // Numbered, bold-named, one line each. The anchor (timestamp/section) is
    // rendered in the same `[anchor]` shape as a claim anchor — the improvement
    // over the April shape, which carried no per-item anchors.
    for (idx, item) in enumeration.items.iter().enumerate() {
        body.push_str(&(idx + 1).to_string());
        body.push_str(". **");
        body.push_str(&prose_line(item.name.trim()));
        body.push_str("**");
        let text = item.text.trim();
        if !text.is_empty() {
            body.push_str(": ");
            body.push_str(&prose_line(text));
        }
        push_anchor(body, item.anchor.as_deref());
        body.push('\n');
    }
    body.push('\n');
}

/// Append ` [anchor]` for a claim or enumeration item. The model copies
/// anchors out of a `[HH:MM:SS]`-prefixed transcript and often keeps the
/// brackets (`'[00:14:45]'`), which the video validator accepts; wrapping that
/// in our own brackets wrote `[[00:14:45]]`, a broken wikilink. Strip any
/// brackets the anchor already carries so exactly one pair is emitted.
fn push_anchor(body: &mut String, anchor: Option<&str>) {
    let Some(anchor) = anchor else {
        return;
    };
    let bare = anchor.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if bare.is_empty() {
        return;
    }
    body.push_str(" [");
    body.push_str(bare);
    body.push(']');
}

fn push_key_ideas(body: &mut String, key_ideas: &[String]) {
    let ideas: Vec<&str> = key_ideas
        .iter()
        .map(|idea| idea.trim())
        .filter(|idea| !idea.is_empty())
        .collect();
    if ideas.is_empty() {
        return;
    }
    body.push_str("## Key Ideas\n\n");
    for idea in ideas {
        body.push_str("- ");
        body.push_str(&prose_line(idea));
        body.push('\n');
    }
    body.push('\n');
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
        // legacy shape) and `(who)` (omitted when absent), joined by a
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
        body.push_str(&prose_line(claim.text.trim()));
        push_anchor(body, claim.anchor.as_deref());
        body.push('\n');
        // Optional verbatim quote as an indented blockquote line beneath the
        // bullet. Flattened so a multi-line quote can't spill into stray lines.
        if let Some(quote) = &claim.quote
            && !quote.trim().is_empty()
        {
            body.push_str("  > \"");
            body.push_str(&prose_line(quote.trim()));
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

/// Render the `## Sessions` footer for a harvested session note: a lead line
/// carrying the repo anchor + message count + date range, then one `clyde://`
/// back-pointer per member session. Per-session title/duration are NOT in the
/// frozen `SessionPayload` - borg's publish layer, which holds the
/// full clustered `SessionRecord`s, is the seam for a richer footer; this keeps
/// the renderer faithful to the typed payload it is handed. Omitted entirely
/// when the payload carries no session ids (nothing to link).
fn push_session_footer(body: &mut String, payload: &SessionPayload) {
    if payload.session_ids.is_empty() {
        return;
    }
    body.push_str("## Sessions\n\n");
    let mut lead_parts: Vec<String> = Vec::new();
    if let Some(repo) = &payload.repo {
        lead_parts.push(format!("repo `{repo}`"));
    }
    if payload.msg_count > 0 {
        lead_parts.push(format!("{} messages", payload.msg_count));
    }
    match (&payload.date_start, &payload.date_end) {
        (Some(start), Some(end)) if start == end => lead_parts.push(start.clone()),
        (Some(start), Some(end)) => lead_parts.push(format!("{start} -> {end}")),
        (Some(start), None) => lead_parts.push(start.clone()),
        (None, Some(end)) => lead_parts.push(end.clone()),
        (None, None) => {}
    }
    if !lead_parts.is_empty() {
        body.push_str(&lead_parts.join(" | "));
        body.push_str("\n\n");
    }
    for id in &payload.session_ids {
        body.push_str("- clyde://");
        body.push_str(id.trim());
        body.push('\n');
    }
    body.push('\n');
}

/// Normalize a URL for the Links-vs-Tools dedup comparison: trimmed, trailing
/// slashes ignored, case-insensitive.
fn url_key(url: &str) -> String {
    url.trim().trim_end_matches('/').to_lowercase()
}

/// The tools that render: `Distilled.tools` with a name, plus the repo
/// payload's `install` line as an `Install` entry unless a tool already
/// carries that exact command. Public so `sb borg eval` grades and shows the
/// judge exactly the tools the note renders.
pub fn tools_to_render(distilled: &Distilled) -> Vec<Tool> {
    let mut tools: Vec<Tool> = distilled
        .tools
        .iter()
        .filter(|t| !t.name.trim().is_empty())
        .cloned()
        .collect();
    if let Some(KindPayload::Repo(p)) = &distilled.kind_specific
        && let Some(install) = p.install.as_deref().map(str::trim)
        && !install.is_empty()
        && !tools
            .iter()
            .any(|t| t.command.as_deref().map(str::trim) == Some(install))
    {
        tools.push(Tool {
            name: "Install".to_string(),
            url: None,
            command: Some(install.to_string()),
        });
    }
    tools
}

fn push_tools(body: &mut String, tools: &[Tool]) {
    if tools.is_empty() {
        return;
    }
    body.push_str("## Tools & Commands\n\n");
    for tool in tools {
        body.push_str("- ");
        body.push_str(&prose_line(tool.name.trim()));
        if let Some(url) = tool.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            body.push(' ');
            body.push_str(url);
        }
        let command = tool.command.as_deref().map(str::trim).filter(|c| !c.is_empty());
        match command {
            Some(cmd) if cmd.contains('\n') => {
                body.push('\n');
                let fence = "`".repeat(longest_backtick_run(cmd).max(2) + 1);
                body.push_str("  ");
                body.push_str(&fence);
                body.push('\n');
                for line in cmd.lines() {
                    body.push_str("  ");
                    body.push_str(line);
                    body.push('\n');
                }
                body.push_str("  ");
                body.push_str(&fence);
            }
            Some(cmd) => {
                let ticks = "`".repeat(longest_backtick_run(cmd) + 1);
                body.push(' ');
                body.push_str(&ticks);
                // CommonMark: pad when the content touches a backtick.
                let pad = cmd.starts_with('`') || cmd.ends_with('`');
                if pad {
                    body.push(' ');
                }
                body.push_str(cmd);
                if pad {
                    body.push(' ');
                }
                body.push_str(&ticks);
            }
            None => {}
        }
        body.push('\n');
    }
    body.push('\n');
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

fn push_links(body: &mut String, links: &[vault::distilled::Link], tools: &[Tool]) {
    let tool_urls: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|t| t.url.as_deref())
        .map(url_key)
        .filter(|u| !u.is_empty())
        .collect();
    let links: Vec<&vault::distilled::Link> = links.iter().filter(|l| !tool_urls.contains(&url_key(&l.url))).collect();
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
