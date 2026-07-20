use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use crate::config::{LinkingConfig, LinkingFilter};
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// The concept glossary + alias table, loaded from `glossary.yml` (Phase 2 of
/// graph-augmented-memory). Mirrors `canonical-tags.yml`: kebab-case concept
/// slugs plus an `aliases` map of surface form → canonical slug.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Glossary {
    pub concepts: Vec<String>,
    pub aliases: HashMap<String, String>,
}

/// Load the glossary from `path`. A missing file yields an empty glossary (the
/// linker simply has no concepts/aliases to apply); a malformed file is a hard
/// error so a typo is not silently ignored.
pub fn load_glossary(path: &Path) -> eyre::Result<Glossary> {
    log::debug!("load_glossary: path={}", path.display());
    if !path.exists() {
        log::info!("load_glossary: {} absent; no glossary concepts/aliases", path.display());
        return Ok(Glossary::default());
    }
    let content = std::fs::read_to_string(path)?;
    let glossary: Glossary = serde_yaml::from_str(&content)?;
    log::info!(
        "load_glossary: {} concepts, {} aliases",
        glossary.concepts.len(),
        glossary.aliases.len()
    );
    Ok(glossary)
}

/// Check if a value is excluded by a filter.
/// Excluded if it matches an exclude pattern and no include pattern overrides it.
fn is_filtered(value: &str, filter: &LinkingFilter) -> bool {
    if filter.exclude.is_empty() {
        return false;
    }
    let excluded = filter.exclude.iter().any(|e| e == value);
    if !excluded {
        return false;
    }
    if !filter.include.is_empty() && filter.include.iter().any(|i| i == value) {
        return false;
    }
    true
}

/// Check if a path is excluded by a filter (glob-style prefix matching).
fn is_path_filtered(path: &Path, filter: &LinkingFilter) -> bool {
    if filter.exclude.is_empty() {
        return false;
    }
    let path_str = path.to_string_lossy();
    let excluded = filter
        .exclude
        .iter()
        .any(|e| path_str.starts_with(e.trim_end_matches('*').trim_end_matches('/')));
    if !excluded {
        return false;
    }
    if !filter.include.is_empty() {
        let included = filter
            .include
            .iter()
            .any(|i| path_str.starts_with(i.trim_end_matches('*').trim_end_matches('/')));
        if included {
            return false;
        }
    }
    true
}

/// Regex to find existing wikilinks (to avoid double-linking).
static EXISTING_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Run wikilink inference on all notes.
pub fn lint_linking(notes: &[Note], config: &LinkingConfig) -> Report {
    let mut report = Report::default();

    // Build entity lists from config + note titles, filtering by target rules
    let note_titles: Vec<(String, String)> = notes
        .iter()
        .filter_map(|n| {
            // Filter by type
            if let Some(ref note_type) = n.frontmatter.note_type
                && is_filtered(note_type, &config.targets.types)
            {
                return None;
            }
            // Filter by path
            if is_path_filtered(&n.path, &config.targets.paths) {
                return None;
            }
            let stem = n.path.file_stem()?.to_str()?.to_string();
            let raw_title = n.frontmatter.title.clone().unwrap_or_else(|| stem.clone());
            // Strip wikilink brackets from titles (some notes have title: "[[foo]]")
            let title = raw_title.trim_start_matches("[[").trim_end_matches("]]").to_string();
            if title.is_empty() {
                return None;
            }
            Some((stem, title))
        })
        .collect();

    let scan_for: HashSet<&str> = config.scan_for.iter().map(|s| s.as_str()).collect();

    for note in notes {
        // Never rewrite the body of the user's own authored notes - their
        // prose is theirs to link by hand, not for the auto-linker to edit.
        if crate::scope::is_authored(note) {
            continue;
        }
        let existing_links = extract_existing_links(&note.body);
        // Lowercase the body + build the offset map ONCE per note, reused by
        // every candidate term below (was rebuilt per (note, term) pair).
        let lowered = LoweredBody::new(&note.body);

        // Match note titles/stems in body text
        if scan_for.contains("concepts") || scan_for.contains("all") {
            for (stem, title) in &note_titles {
                // Don't self-link
                if note.path.file_stem().and_then(|s| s.to_str()) == Some(stem) {
                    continue;
                }

                // Don't suggest if already linked
                if existing_links.contains(&stem.to_lowercase()) {
                    continue;
                }

                // Check if the title or stem appears in the body (case-insensitive)
                if let Some((context, surface)) = lowered.find_mention(title, stem, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.concept".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{title}' could be linked as [[{stem}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: stem.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match known people entities
        if scan_for.contains("people") || scan_for.contains("all") {
            for person in &config.entities.people {
                if existing_links.contains(&person.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) = lowered.find_mention(person, person, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.person".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{person}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: person.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match known project entities
        if scan_for.contains("projects") || scan_for.contains("all") {
            for project in &config.entities.projects {
                if existing_links.contains(&project.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) = lowered.find_mention(project, project, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.project".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{project}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: project.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }

        // Match glossary concepts (Phase 2): kebab-case slugs linked at first
        // body mention as [[slug]] (or [[slug|Surface]] when the prose case
        // differs). Plus alias surface forms -> canonical slug as piped links.
        if scan_for.contains("concepts") || scan_for.contains("all") {
            for slug in &config.entities.concepts {
                if existing_links.contains(&slug.to_lowercase()) {
                    continue;
                }
                if note.path.file_stem().and_then(|s| s.to_str()) == Some(slug.as_str()) {
                    continue; // never self-link a concept's own hub note
                }
                if let Some((context, surface)) = lowered.find_mention(slug, slug, config.min_word_length) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.glossary".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{surface}' could be linked as [[{slug}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: slug.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }

            for (alias_surface, slug) in &config.aliases {
                if existing_links.contains(&slug.to_lowercase()) {
                    continue;
                }
                if let Some((context, surface)) =
                    lowered.find_mention(alias_surface, alias_surface, config.min_word_length)
                {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.alias".to_string(),
                        severity: Severity::Info,
                        message: format!("alias '{surface}' could be linked as [[{slug}|{surface}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: slug.clone(),
                            surface,
                            context,
                        }),
                    });
                }
            }
        }
    }

    log::info!("linking lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply link suggestions: insert [[wikilinks]] at first mention.
///
/// Returns the real, byte-changed paths this call actually wrote. This is
/// the ONLY source the daemon's oscillation fingerprint may draw from for
/// the `link` action: `lint_linking`'s suggestion paths are NOT all
/// appliable - `find_mention` (detection) and `insert_first_wikilink`
/// (mutation) can disagree on a differently-sliced body, so a reported
/// suggestion can leave `new_content == content` and never write.
pub fn apply_linking(vault_root: &Path, notes: &[Note], config: &LinkingConfig) -> eyre::Result<Vec<String>> {
    log::debug!(
        "linking::apply_linking: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let report = lint_linking(notes, config);
    let mut written = Vec::new();

    // Group fixes by file, carrying (target, surface) so the apply step can
    // emit a piped link that preserves the prose wording.
    let mut fixes_by_path: std::collections::HashMap<&std::path::Path, Vec<(&str, &str)>> =
        std::collections::HashMap::new();
    for violation in &report.violations {
        if let Some(Fix::AddWikilink { target, surface, .. }) = &violation.fix {
            fixes_by_path
                .entry(violation.path.as_path())
                .or_default()
                .push((target, surface));
        }
    }

    for (path, fixes) in &fixes_by_path {
        let abs_path = vault_root.join(path);
        let content = std::fs::read_to_string(&abs_path)?;
        let mut new_content = content.clone();

        for (target, surface) in fixes {
            // Find the first mention of the surface form and wrap it in [[]].
            if let Some(new) = insert_first_wikilink(&new_content, target, surface) {
                new_content = new;
            }
        }

        if new_content != content {
            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
            log::info!("inserted wikilinks: {}", path.display());
            written.push(path.to_string_lossy().to_string());
        }
    }

    log::debug!("linking::apply_linking: written={}", written.len());
    Ok(written)
}

/// Extract all existing wikilink targets from body (lowercased).
fn extract_existing_links(body: &str) -> HashSet<String> {
    EXISTING_LINK_RE
        .captures_iter(body)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_lowercase()))
        .collect()
}

/// A note body lowercased ONCE, with a map from lowercased-byte offsets back to
/// original `body` byte offsets. Built once per note and reused across every
/// candidate term (`find_mention`). Previously the lowercase pass + offset map
/// were rebuilt on every (note, term) pair - O(notes x terms x body), and the
/// daemon runs `lint_linking` every sweep. Hoisting the per-note build out of
/// the term loop drops the inner cost to a single `find` per term.
struct LoweredBody<'a> {
    body: &'a str,
    body_lower: String,
    /// `(lower_off, body_off)` pairs, ascending, recorded at the start of every
    /// body char's lowercase expansion plus an end sentinel. `to_lowercase` can
    /// change byte length (e.g. Turkish I), so a `body_lower.find()` offset is
    /// NOT a valid index into `body`; mapping back keeps every slice on a real
    /// char boundary and yields the correct original-case span.
    map: Vec<(usize, usize)>,
}

impl<'a> LoweredBody<'a> {
    fn new(body: &'a str) -> Self {
        let mut body_lower = String::with_capacity(body.len());
        let mut map: Vec<(usize, usize)> = Vec::new();
        for (body_off, ch) in body.char_indices() {
            map.push((body_lower.len(), body_off));
            for lc in ch.to_lowercase() {
                body_lower.push(lc);
            }
        }
        map.push((body_lower.len(), body.len()));
        Self { body, body_lower, map }
    }

    fn to_body(&self, lower_off: usize) -> usize {
        match self.map.binary_search_by_key(&lower_off, |&(l, _)| l) {
            Ok(i) => self.map[i].1,
            // A match landing mid-expansion (rare) snaps back to that char's start.
            Err(i) => self.map[i.saturating_sub(1)].1,
        }
    }

    /// Find a case-insensitive mention of `title` (then `stem`) in the body.
    /// Returns `(context, surface)` where `surface` is the text actually matched
    /// (preserving original case) so the caller can emit `[[target|surface]]`.
    fn find_mention(&self, title: &str, stem: &str, min_len: usize) -> Option<(String, String)> {
        let body = self.body;
        for term in [title, stem] {
            let term_lower = term.to_lowercase();
            if term_lower.len() < min_len {
                continue;
            }

            // Iterate every occurrence and return the first CLEAN one (not
            // inside an existing wikilink, not mid-word, not inside a
            // structural construct). Single-find would stop at a structural
            // hit and either suppress the term or - via the independent mutation
            // path - corrupt it.
            for (lpos, _) in self.body_lower.match_indices(&term_lower) {
                let pos = self.to_body(lpos);
                let after_pos = self.to_body(lpos + term_lower.len());

                // Detection and mutation must agree on what is a clean,
                // appliable mention - `is_clean_mention` is the single
                // arbiter both sides call (see its doc comment).
                if !is_clean_mention(body, pos, after_pos) {
                    log::trace!("skipping unclean mention: {term}");
                    continue;
                }

                // The surface form is the original-case substring at the match.
                let surface = body[pos..after_pos].to_string();
                // Context: surrounding ~20 chars, snapped to char boundaries.
                let start = body.floor_char_boundary(pos.saturating_sub(20));
                let end = body.ceil_char_boundary((after_pos + 20).min(body.len()));
                let context = body[start..end].to_string();
                return Some((context, surface));
            }
        }

        None
    }
}

/// Characters that bound a "token" for URL detection: whitespace plus the
/// delimiters that wrap URLs in Markdown/HTML (quotes, angle brackets, parens,
/// backticks). The maximal run of non-boundary chars around a match is the
/// token tested for URL-ness.
fn is_token_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '(' | ')' | '`')
}

/// Byte bounds `(line_start, line_end)` of the line containing `pos`.
fn line_bounds(text: &str, pos: usize) -> (usize, usize) {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
    (line_start, line_end)
}

/// True if `[start, end)` sits inside a URL token: the surrounding non-boundary
/// run contains `://`, starts `www.`, carries a `scheme:` prefix (mailto:, …),
/// or the match is immediately followed by `/` or `?` (a bare path).
fn in_url_token(text: &str, start: usize, end: usize) -> bool {
    let mut tok_start = start;
    while tok_start > 0 {
        let prev = text[..tok_start].chars().next_back().expect("non-empty prefix");
        if is_token_boundary(prev) {
            break;
        }
        tok_start -= prev.len_utf8();
    }
    let mut tok_end = end;
    while tok_end < text.len() {
        let next = text[tok_end..].chars().next().expect("non-empty suffix");
        if is_token_boundary(next) {
            break;
        }
        tok_end += next.len_utf8();
    }
    let token = &text[tok_start..tok_end];
    if token.contains("://") || token.to_ascii_lowercase().starts_with("www.") {
        return true;
    }
    if let Some(colon) = token.find(':') {
        let scheme = &token[..colon];
        if !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        {
            return true;
        }
    }
    matches!(text[end..].chars().next(), Some('/') | Some('?'))
}

/// True if `pos` sits inside a Markdown link/image destination or title:
/// after a `](` with no closing `)` yet on the same line.
fn in_link_destination(text: &str, pos: usize) -> bool {
    let (ls, _) = line_bounds(text, pos);
    let before = &text[ls..pos];
    match (before.rfind("]("), before.rfind(')')) {
        (Some(open), close) => close.is_none_or(|c| c < open),
        (None, _) => false,
    }
}

/// True if `pos` sits inside code: a fenced block (odd count of ``` fences
/// above), an indented (4-space / tab) code line, or an inline `` `code` ``
/// span (odd backtick count before `pos` on the line).
fn in_code_context(text: &str, pos: usize) -> bool {
    let (ls, _) = line_bounds(text, pos);
    let fences = text[..ls].lines().filter(|l| l.trim_start().starts_with("```")).count();
    if fences % 2 == 1 {
        return true;
    }
    let line = &text[ls..];
    if line.starts_with("    ") || line.starts_with('\t') {
        return true;
    }
    text[ls..pos].matches('`').count() % 2 == 1
}

/// True if `pos` sits inside math: a `$$` block (odd count of `$$`-only lines
/// above) or an inline `$…$` span (odd `$` count before `pos` on the line).
fn in_math(text: &str, pos: usize) -> bool {
    let (ls, _) = line_bounds(text, pos);
    let blocks = text[..ls].lines().filter(|l| l.trim() == "$$").count();
    if blocks % 2 == 1 {
        return true;
    }
    text[ls..pos].matches('$').count() % 2 == 1
}

/// True if `pos` sits inside an HTML tag/attribute, an autolink `<…>`, or an
/// HTML comment `<!-- … -->`, scoped to the current line.
fn in_html_tag_or_comment(text: &str, pos: usize) -> bool {
    let (ls, _) = line_bounds(text, pos);
    let before = &text[ls..pos];
    if let Some(c_open) = before.rfind("<!--")
        && !before[c_open..].contains("-->")
    {
        return true;
    }
    match (before.rfind('<'), before.rfind('>')) {
        (Some(lt), gt) => gt.is_none_or(|g| g < lt),
        (None, _) => false,
    }
}

/// True if `pos` sits inside an existing `[[wikilink]]`: the last `[[` before
/// `pos` is not yet closed by a `]]`. Linking a term that appears inside another
/// link's target OR its display text builds a broken NESTED wikilink, so this is
/// off-limits. The `]]`-is-None case (no prior close, e.g. a match in the very
/// first link's display) is the hole the old `(Some, Some)` guard missed.
fn in_wikilink(text: &str, pos: usize) -> bool {
    let before = &text[..pos];
    match (before.rfind("[["), before.rfind("]]")) {
        (Some(o), c) => c.is_none_or(|c| o > c),
        (None, _) => false,
    }
}

/// True if the byte range `[start, end)` in `text` sits inside a Markdown/HTML
/// structural construct where a wikilink must never be inserted. `text` is
/// whatever slice the caller scans (the post-frontmatter body for both call
/// sites), and the offsets are into THAT slice - detection and mutation must
/// pass matching slice+offsets so the two never disagree.
fn inside_structure(text: &str, start: usize, end: usize) -> bool {
    in_wikilink(text, start)
        || in_code_context(text, start)
        || in_html_tag_or_comment(text, start)
        || in_math(text, start)
        || in_url_token(text, start, end)
        || in_link_destination(text, start)
}

/// Word-char definition matching the Unicode-aware `\w`/`\b` semantics the
/// `regex` crate uses by default: alphanumeric or underscore. Detection used
/// to test only `is_ascii_alphanumeric` while mutation used a regex `\b` -
/// they disagreed on an underscore or non-ASCII-letter boundary (`foo_bar`,
/// `café`), so a mention detection reported could never be re-matched by
/// mutation. Both sides now share this one definition.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True if the byte range `[start, end)` in `body` is a clean, appliable
/// mention: word-bounded on both sides, not inside an existing wikilink (nor
/// immediately before one's closing `]]`), and not inside a structural
/// construct (`inside_structure`).
///
/// THE single arbiter of "is this surface linkable here", shared by
/// detection (`LoweredBody::find_mention`, used by `lint_linking`) and
/// mutation (`insert_first_wikilink`). The two previously disagreed - an
/// ASCII-only boundary check in detection vs. a Unicode `\b` in mutation,
/// each also independently re-deriving the wikilink/structure guard - which
/// let `lint_linking` report a mention `insert_first_wikilink` could never
/// re-match: `new_content == content`, no write, and the same suggestion
/// re-reported forever (the perpetual `link: N files` phantom). Both call
/// sites now pass the SAME body slice (see `insert_first_wikilink`, which
/// now derives its body via `vault::frontmatter::split_raw`, the same
/// splitter that produces `Note::body`).
fn is_clean_mention(body: &str, start: usize, end: usize) -> bool {
    let before_is_word = body[..start].chars().next_back().is_some_and(is_word_char);
    let after_is_word = body[end..].chars().next().is_some_and(is_word_char);
    if before_is_word || after_is_word {
        return false;
    }
    if in_wikilink(body, start) || body[end..].starts_with("]]") {
        return false;
    }
    !inside_structure(body, start, end)
}

/// Insert a wikilink at the first CLEAN mention of `surface` in content,
/// pointing at `target`. Only searches the body (after frontmatter) to avoid
/// corrupting YAML fields. Emits a piped link `[[target|matched]]` when the
/// matched text differs from `target` (an alias surface form, or a title whose
/// stem differs); a plain `[[matched]]` otherwise.
fn insert_first_wikilink(content: &str, target: &str, surface: &str) -> Option<String> {
    log::debug!(
        "insert_first_wikilink: target={target} surface={surface} content_len={}",
        content.len()
    );
    // Find the end of frontmatter so we only search the body - via the SAME
    // shared splitter (`vault::frontmatter::split_raw`) that produces
    // `Note::body`, so detection and mutation see identical body text. The
    // previous ad hoc `find("\n---")` search here was a second, buggier
    // reimplementation of exactly the split `split_raw` already owns (a bare
    // `find` can mis-split on a frontmatter value containing a literal
    // `---` line; `split_raw` requires the closing delimiter to be a full
    // line).
    let body_start = match vault::frontmatter::split_raw(content) {
        Some((_, body)) => body.as_ptr() as usize - content.as_ptr() as usize,
        None => 0,
    };
    let body = &content[body_start..];

    // Match the surface form (the text that actually appears) rather than the
    // target slug, so an alias like "Retrieval-Augmented Generation" is found
    // even though it points at the slug "rag". No `\b` here - boundary is
    // decided by the shared `is_clean_mention` below, the same predicate
    // detection uses.
    let pattern = format!(r"(?i){}", regex::escape(surface));
    let re = Regex::new(&pattern).ok()?;

    // Wrap the FIRST occurrence that `is_clean_mention` accepts. Detection
    // (`find_mention`) and mutation (here) locate candidate positions
    // independently (substring scan vs. regex), but both hand every
    // candidate to the SAME clean/appliable predicate, so they can no
    // longer disagree on which mentions are linkable.
    for mat in re.find_iter(body) {
        if !is_clean_mention(body, mat.start(), mat.end()) {
            continue;
        }

        let abs_start = body_start + mat.start();
        let abs_end = body_start + mat.end();
        let before = &content[..abs_start];
        let matched = &content[abs_start..abs_end];
        let after = &content[abs_end..];
        // Plain link only when the matched text is exactly the slug; otherwise
        // pipe so the canonical slug is the link target while the prose wording
        // (case, alias surface form) is preserved as the display text.
        let link = if matched == target {
            format!("[[{matched}]]")
        } else {
            format!("[[{target}|{matched}]]")
        };
        log::debug!("insert_first_wikilink: matched at body-relative offset {}", mat.start());
        return Some(format!("{before}{link}{after}"));
    }
    log::debug!("insert_first_wikilink: no clean mention found");
    None
}

#[cfg(test)]
mod tests;
