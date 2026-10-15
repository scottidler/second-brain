use std::sync::LazyLock;

use crate::config::LinkConfig;
use crate::types::{ContentKind, IngestResult, IngestStatus};

static URL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"https?://\S+").expect("valid regex"));

const RESOLUTIONS: &[(&str, (usize, usize))] = &[
    ("nHD", (640, 360)),
    ("FWVGA", (854, 480)),
    ("SD", (1280, 720)),
    ("FHD", (1920, 1080)),
    ("4K", (3840, 2160)),
];

const SHORTS_RESOLUTIONS: &[(&str, (usize, usize))] =
    &[("480p", (480, 854)), ("720p", (720, 1280)), ("1080p", (1080, 1920))];

#[derive(Debug, PartialEq)]
pub struct UrlMatch {
    pub url: String,
    pub link_name: String,
    pub width: usize,
    pub height: usize,
}

impl UrlMatch {
    pub fn is_youtube_type(&self) -> bool {
        matches!(self.link_name.as_str(), "youtube" | "shorts")
    }

    pub fn is_shorts(&self) -> bool {
        self.link_name == "shorts"
    }
}

/// Classify a pre-normalized URL against link config patterns.
/// The URL should already be cleaned and canonicalized before calling this.
pub fn classify_url(normalized_url: &str, links: &[LinkConfig]) -> eyre::Result<UrlMatch> {
    for link in links {
        let re = regex::Regex::new(&link.regex)?;
        if re.is_match(normalized_url) {
            let is_shorts = link.name == "shorts";
            let (width, height) = resolve_dimensions(&link.resolution, is_shorts);
            return Ok(UrlMatch {
                url: normalized_url.to_string(),
                link_name: link.name.clone(),
                width,
                height,
            });
        }
    }

    // Should not happen if config has a catch-all, but fallback
    Ok(UrlMatch {
        url: normalized_url.to_string(),
        link_name: "default".to_string(),
        width: 854,
        height: 480,
    })
}

fn resolve_dimensions(resolution: &str, is_shorts: bool) -> (usize, usize) {
    let table = if is_shorts { SHORTS_RESOLUTIONS } else { RESOLUTIONS };
    table
        .iter()
        .find(|(name, _)| *name == resolution)
        .map(|(_, dims)| *dims)
        .unwrap_or(if is_shorts { (480, 854) } else { (854, 480) })
}

pub fn extract_url_from_text(text: &str) -> Option<String> {
    URL_REGEX.find(text).map(|m| {
        m.as_str()
            .trim_end_matches(['.', ',', ')', ']', '>', ';', '!'])
            .to_string()
    })
}

/// True for a line the Android share sheet adds: `Source: <site>`, nothing else.
fn is_share_boilerplate_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() > "source:".len()
        && trimmed.is_char_boundary("source:".len())
        && trimmed[.."source:".len()].eq_ignore_ascii_case("source:")
}

/// Drop every share-sheet boilerplate line (see [`is_share_boilerplate_line`])
/// from `text`, keeping the remaining lines and their line breaks. The one
/// boilerplate rule, shared by ingest ([`extract_capture_note`]) and the
/// `--force` reingest preserve path, so an old boilerplate note is not carried
/// forward. An all-boilerplate or blank result is `None`.
pub fn strip_share_boilerplate(text: &str) -> Option<String> {
    let kept = text
        .lines()
        .filter(|line| !is_share_boilerplate_line(line))
        .collect::<Vec<_>>()
        .join("\n");
    match kept.trim() {
        "" => None,
        trimmed => Some(trimmed.to_string()),
    }
}

/// Drop a capture note that merely repeats the fetched page title (share
/// sheets and bookmarkers paste the title as the "note"). Compared
/// case-insensitively on whitespace-collapsed text. Runs at publish, because
/// the title only exists after the fetch. An empty result is `None`.
pub fn drop_note_equal_to_title<'a>(note: Option<&'a str>, title: &str) -> Option<&'a str> {
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let note = note.map(str::trim).filter(|n| !n.is_empty())?;
    (collapse(note) != collapse(title)).then_some(note)
}

/// The one capture-note extraction rule, shared by every transport (telegram,
/// signal, ntfy, discord, CLI text). Given the raw message `text` and the
/// already-extracted first `url`, the capture note is the message text with the
/// FIRST whitespace token that contains that URL removed, then
/// whitespace-collapsed. Empty result -> `None`. Any additional URLs stay in
/// the note text as plain links; the first URL remains the capture target (as
/// today). This is the single source of truth for the rule so no transport can
/// drift from the others.
///
/// Android share-sheet boilerplate (`Source: <site>` lines) is not intent and is
/// dropped before whitespace is collapsed, because collapsing would erase the
/// line boundary the rule needs. See [`is_share_boilerplate_line`].
pub fn extract_capture_note(text: &str, url: &str) -> Option<String> {
    let mut removed = false;
    let intent_lines = strip_share_boilerplate(text)?;
    let note = intent_lines
        .split_whitespace()
        .filter(|token| {
            if !removed && token.contains(url) {
                removed = true;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    match note.trim() {
        "" => None,
        trimmed => Some(trimmed.to_string()),
    }
}

/// Build a URL `ContentKind` from a text-bearing transport message (telegram,
/// discord, signal): the first URL is the capture target, the surrounding
/// prose becomes the capture note via [`extract_capture_note`]. Returns the
/// content plus a display string (the URL). `None` when the text has no URL.
/// This is the single wiring point so every text transport applies the exact
/// same capture-note rule.
pub fn url_content_from_text(text: &str) -> Option<(ContentKind, String)> {
    let url = extract_url_from_text(text)?;
    let note = extract_capture_note(text, &url);
    let display = url.clone();
    Some((ContentKind::Url { url, note }, display))
}

pub fn format_reply(result: &IngestResult, url: &str) -> String {
    let elapsed = result.elapsed_secs.map(|s| format!(" ({:.1}s)", s)).unwrap_or_default();
    let prefix = result
        .trace_id
        .as_ref()
        .map(|tid| format!("[{tid}] "))
        .unwrap_or_default();

    match &result.status {
        IngestStatus::Completed => {
            let title = result.title.as_deref().unwrap_or("Untitled");
            let tags = if result.tags.is_empty() {
                String::new()
            } else {
                format!(
                    "\nTags: {}",
                    result
                        .tags
                        .iter()
                        .map(|t| format!("#{t}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            format!("{prefix}Saved: {title}{elapsed}{tags}")
        }
        IngestStatus::Duplicate { original_date } => {
            format!("{prefix}Duplicate{elapsed}: already ingested on {original_date}\nURL: {url}")
        }
        IngestStatus::Failed { reason } => {
            format!("{prefix}Failed{elapsed}: {reason}\nURL: {url}")
        }
        IngestStatus::Queued => format!("{prefix}Queued for processing."),
    }
}

#[cfg(test)]
mod tests;
