//! Deterministic hub-body assembly
//! (`docs/design/2026-08-15-entity-hub-two-vector-synthesis.md`, Phase 2).
//!
//! A hub body is its members' already-distilled claims, grouped by ingestion
//! vector, quoted and wikilinked. No LLM: the claims exist because the L2
//! distill contract produced them, so the hub is an ARRANGEMENT of them, never a
//! rewrite. That makes the body honest (it can only say what a member note says,
//! with the link to prove it), idempotent (a pure function of membership +
//! claims, so unchanged inputs write zero bytes), retrievable (the claim text
//! lands in the hub's FTS row and its embedding), and free to re-run.
//!
//! Everything here is a pure function over `&[HubMember]`. That is the
//! structural fix for how the previous mechanism shipped broken: its tests
//! injected a synthesizer double whose `_members` argument was ignored, so
//! "synthesis" could be fed bare file paths and still pass green.

use std::str::FromStr;

use vault::distilled::Claim;
use vault::schema::NoteType;

use crate::config::RenderConfig;

/// Which ingestion vector a member belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vector {
    /// External content: youtube / article / github / social / research.
    Source,
    /// A harvested Claude Code session.
    Session,
    /// Neither (image, pdf, note, ...) - excluded from both body sections.
    Other,
}

impl Vector {
    /// Classify a member's raw `type:` frontmatter value through
    /// `vault::schema::NoteType` - the single source of truth for note types,
    /// never a local list of strings. An unparseable / absent type is `Other`,
    /// which excludes the member from both sections without failing the run.
    pub fn of(note_type: &str) -> Self {
        match NoteType::from_str(note_type.trim()) {
            Ok(NoteType::Session) => Vector::Session,
            Ok(NoteType::Youtube | NoteType::Article | NoteType::Github | NoteType::Social | NoteType::Research) => {
                Vector::Source
            }
            _ => Vector::Other,
        }
    }
}

/// One loaded member note: everything the renderer needs and nothing else.
#[derive(Debug, Clone)]
pub struct HubMember {
    /// Vault-relative path, e.g. `knowledge/tech/foo.md`.
    pub path: String,
    /// Display title from frontmatter (falls back to the file stem at load).
    pub title: String,
    /// Raw `type:` frontmatter value.
    pub note_type: String,
    /// `date:` frontmatter, the primary sort key. `None` sorts last.
    pub date: Option<String>,
    /// Claims parsed out of the member's `## Claims` section, in note order.
    pub claims: Vec<Claim>,
}

impl HubMember {
    fn vector(&self) -> Vector {
        Vector::of(&self.note_type)
    }

    /// The Obsidian wikilink markup that resolves to this member. The TARGET is
    /// the full vault-relative path minus `.md` (a literal-path match resolves
    /// unconditionally and cannot be ambiguous across two same-basename notes),
    /// aliased to the title for a readable render. A title carrying wikilink
    /// syntax (`|`, `[`, `]`) drops the alias rather than emitting broken markup.
    fn wikilink(&self) -> String {
        let target = self.path.strip_suffix(".md").unwrap_or(&self.path);
        let title = self.title.trim();
        if title.is_empty() || title.contains(['|', '[', ']']) {
            format!("[[{target}]]")
        } else {
            format!("[[{target}|{title}]]")
        }
    }
}

/// Order members for rendering: `date:` DESCENDING, path ASCENDING as the
/// tiebreak, and a member without `date:` sorts last.
///
/// The key has to be both stable per member and relevance-correlated. Claim
/// count is a relevance proxy but mutates on every re-distill, so unrelated work
/// would rewrite a 400-member hub; a bare path is stable but arbitrary, so a
/// capped mega-hub would render whatever sorts alphabetically first. `date:` is
/// both: the schema convention preserves it across reingest and recency is a
/// real relevance signal for sources. Sessions are batch-harvested and carry few
/// distinct dates, so their order degenerates to the path tiebreak within a
/// batch - accepted, and still strictly better than arbitrary on both vectors.
fn sort_members(members: &mut [&HubMember]) {
    members.sort_by(|a, b| match (&a.date, &b.date) {
        (Some(x), Some(y)) => y.cmp(x).then_with(|| a.path.cmp(&b.path)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.path.cmp(&b.path),
    });
}

/// One vector's rendering inputs: its full claim-bearing membership count (the
/// number the definition sentence states) and the capped members that render.
struct Section<'a> {
    /// FULL claim-bearing membership, not the capped set.
    total: usize,
    capped: Vec<&'a HubMember>,
}

impl<'a> Section<'a> {
    fn build(members: &'a [HubMember], vector: Vector, max_members: usize) -> Self {
        let mut claim_bearing: Vec<&HubMember> = members
            .iter()
            .filter(|m| m.vector() == vector && !m.claims.is_empty())
            .collect();
        sort_members(&mut claim_bearing);
        let total = claim_bearing.len();
        claim_bearing.truncate(max_members);
        Self {
            total,
            capped: claim_bearing,
        }
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The claims that render, in body order: capped members in sort order,
    /// each contributing at most `max_claims` claims in note order. The digest
    /// draws from exactly this sequence - never a second selection rule.
    fn claim_texts(&self, max_claims: usize) -> Vec<&str> {
        self.capped
            .iter()
            .flat_map(|m| m.claims.iter().take(max_claims))
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
            .collect()
    }
}

/// Render a hub body from its loaded members, or `None` when no member carries
/// a claim under either vector (the caller resets the hub to its stub).
///
/// Pure and total: same members + same caps => byte-identical output.
pub fn render_hub_body(title: &str, members: &[HubMember], caps: &RenderConfig) -> Option<String> {
    log::debug!(
        "cortex::hub::render: title={title} members={} max_members={} max_claims={} budget={}",
        members.len(),
        caps.max_members_per_section,
        caps.max_claims_per_member,
        caps.summary_byte_budget
    );
    let sources = Section::build(members, Vector::Source, caps.max_members_per_section);
    let sessions = Section::build(members, Vector::Session, caps.max_members_per_section);
    if sources.is_empty() && sessions.is_empty() {
        return None;
    }

    let mut body = String::new();
    body.push_str("## Summary\n\n");
    body.push_str(&render_digest(title, &sources, &sessions, caps));
    body.push('\n');
    push_section(&mut body, "## From sources", &sources, caps.max_claims_per_member);
    push_section(
        &mut body,
        "## From your sessions",
        &sessions,
        caps.max_claims_per_member,
    );
    Some(body.trim_end().to_string())
}

/// Emit one `## From ...` section, or nothing when the vector is absent.
fn push_section(body: &mut String, heading: &str, section: &Section, max_claims: usize) {
    if section.is_empty() {
        return;
    }
    body.push_str(heading);
    body.push_str("\n\n");
    for member in &section.capped {
        let link = member.wikilink();
        for claim in member.claims.iter().take(max_claims) {
            let text = claim.text.trim();
            if text.is_empty() {
                continue;
            }
            body.push_str("- ");
            body.push_str(text);
            body.push_str(" (");
            body.push_str(&link);
            body.push_str(")\n");
        }
    }
    let hidden = section.total - section.capped.len();
    if hidden > 0 {
        body.push_str(&format!("...and {hidden} more claim-bearing members\n"));
    }
    body.push('\n');
}

/// The `## Summary` digest: a static definition sentence, then one line per
/// present vector, sessions first.
///
/// The digest is the hub's ONLY embedding surface (cortex embeds
/// `title + capture_note + summary`, claim embeddings are globally off, and the
/// live pipeline is vector-only), so both vectors have to reach it or the
/// scarcer one is invisible to retrieval. Sessions lead because they are the
/// scarcer vector on every large hub and the tail is what truncation eats.
fn render_digest(title: &str, sources: &Section, sessions: &Section, caps: &RenderConfig) -> String {
    let definition = definition_sentence(title, sources.total, sessions.total);
    let mut digest = String::with_capacity(caps.summary_byte_budget);
    digest.push_str(&definition);
    digest.push('\n');

    // 2. The definition sentence AND its newline are always emitted and count
    //    first; everything else is measured in UTF-8 bytes of the exact text.
    let remaining = caps.summary_byte_budget.saturating_sub(digest.len());
    // 3/4. One vector present takes all of `remaining`; both present split it
    //      with integer division and the odd byte going to sources.
    let (sessions_budget, mut sources_budget) = match (sessions.is_empty(), sources.is_empty()) {
        (false, false) => {
            let s = remaining / 2;
            (s, remaining - s)
        }
        (false, true) => (remaining, 0),
        (true, false) => (0, remaining),
        (true, true) => (0, 0),
    };

    if !sessions.is_empty() {
        let line = vector_line(
            "Sessions",
            &sessions.claim_texts(caps.max_claims_per_member),
            sessions_budget,
        );
        // 5. Unused session budget cedes to sources, ONE-directionally: no
        //    lookahead, no second pass. Unused SOURCE slack is the accepted
        //    price of that determinism.
        sources_budget += sessions_budget - line.len();
        digest.push_str(&line);
    }
    if !sources.is_empty() {
        digest.push_str(&vector_line(
            "Sources",
            &sources.claim_texts(caps.max_claims_per_member),
            sources_budget,
        ));
    }
    digest
}

/// `<Title>: hub of N sources and M sessions.` - deterministic, ends with a
/// period, and therefore exactly what `first_sentence` returns for the five
/// oracle handlers that default to `DetailLevel::Tldr`.
///
/// `N`/`M` are the FULL claim-bearing membership counts, NOT the capped set:
/// "hub of 20 sources" on a 408-member hub would be a false statement on the
/// surface five tools render. A ONE-VECTOR hub names only the vector it has -
/// never "and 0 sessions".
fn definition_sentence(title: &str, sources: usize, sessions: usize) -> String {
    let src = format!("{sources} {}", plural(sources, "source"));
    let ses = format!("{sessions} {}", plural(sessions, "session"));
    match (sources, sessions) {
        (0, 0) => format!("{title}: hub of no claim-bearing members."),
        (0, _) => format!("{title}: hub of {ses}."),
        (_, 0) => format!("{title}: hub of {src}."),
        (_, _) => format!("{title}: hub of {src} and {ses}."),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

/// One digest line (`Sessions: a; b; c\n`) built to fit `budget` UTF-8 bytes.
///
/// A line's cost is the FULL emitted text: the label, every claim, every `; `
/// joiner, and the trailing newline. Single forward pass, whole claims
/// preferred: append while the line still fits, stop at the first claim that
/// does not (no lookahead). The one exception is a FIRST claim that overflows on
/// its own - dropping it would zero a real vector (the longest live member
/// claims blob is 3460 bytes), so it is truncated at a UTF-8 character boundary
/// such that the line INCLUDING a literal `...` still fits. Returns `""` when
/// not even a truncated first claim fits, so the caller emits no line at all.
fn vector_line(label: &str, claims: &[&str], budget: usize) -> String {
    const ELLIPSIS: &str = "...";
    let prefix = format!("{label}: ");
    // label + ": " + at least one byte of claim + "\n"
    if claims.is_empty() || budget <= prefix.len() + 1 {
        return String::new();
    }
    let mut line = prefix;
    let mut count = 0usize;
    for claim in claims {
        let joiner = if count == 0 { "" } else { "; " };
        let cost = line.len() + joiner.len() + claim.len() + 1; // +1 = trailing newline
        if cost <= budget {
            line.push_str(joiner);
            line.push_str(claim);
            count += 1;
            continue;
        }
        if count == 0 {
            // Truncate the first claim so `line + head + "..." + "\n"` fits.
            let room = budget.saturating_sub(line.len() + ELLIPSIS.len() + 1);
            let head = truncate_on_char_boundary(claim, room);
            if head.is_empty() {
                return String::new();
            }
            line.push_str(head);
            line.push_str(ELLIPSIS);
            count += 1;
        }
        break;
    }
    if count == 0 {
        return String::new();
    }
    line.push('\n');
    line
}

/// Longest prefix of `text` that is at most `max_bytes` long and ends on a UTF-8
/// character boundary (never mid code point).
fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
