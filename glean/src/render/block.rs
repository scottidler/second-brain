//! Single-fencepost merge primitive.
//!
//! Splits a chunk file into (preamble, auto-body, postamble) on the
//! `glean:fencepost-start` / `glean:fencepost-end` markers. Replaces
//! the auto-body with the freshly-rendered body; preserves preamble
//! and postamble byte-for-byte. If the existing file has no fenceposts
//! (operator deleted them or the file is fresh), the existing content
//! is preserved as the preamble and the new auto-body is appended.
//! If the new render has no fenceposts (programmer error), the merge
//! falls back to returning the new content verbatim.

const FENCEPOST_START: &str = "<!-- glean:fencepost-start -->";
const FENCEPOST_END: &str = "<!-- glean:fencepost-end -->";

/// Merge a fresh `template` body into an `existing` chunk file.
///
/// - If both files have fenceposts: keep `existing`'s preamble and
///   postamble; swap in `template`'s auto-body.
/// - If only `template` has fenceposts: treat the whole `existing`
///   file as preamble (operator hand-edited the body without the
///   fenceposts; we preserve their content and append the new body).
/// - If `template` has no fenceposts: return `template` verbatim.
pub fn merge(existing: &str, template: &str) -> String {
    let Some(template_segments) = split_fencepost(template) else {
        log::warn!("render::block::merge: template has no fenceposts; returning verbatim");
        return template.to_string();
    };
    if let Some(existing_segments) = split_fencepost(existing) {
        format!(
            "{preamble}{start}\n{body}{end}\n{postamble}",
            preamble = existing_segments.preamble,
            start = FENCEPOST_START,
            body = template_segments.auto_body,
            end = FENCEPOST_END,
            postamble = existing_segments.postamble
        )
    } else {
        log::info!("render::block::merge: existing has no fenceposts; preserving as preamble");
        let mut out = existing.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(FENCEPOST_START);
        out.push('\n');
        out.push_str(&template_segments.auto_body);
        out.push_str(FENCEPOST_END);
        out.push('\n');
        out
    }
}

struct Segments {
    preamble: String,
    auto_body: String,
    postamble: String,
}

fn split_fencepost(s: &str) -> Option<Segments> {
    let start_idx = s.find(FENCEPOST_START)?;
    let after_start = start_idx + FENCEPOST_START.len();
    let body_start = if s.as_bytes().get(after_start) == Some(&b'\n') {
        after_start + 1
    } else {
        after_start
    };
    let end_rel = s[body_start..].find(FENCEPOST_END)?;
    let end_abs = body_start + end_rel;
    let after_end = end_abs + FENCEPOST_END.len();
    let postamble_start = if s.as_bytes().get(after_end) == Some(&b'\n') {
        after_end + 1
    } else {
        after_end
    };
    Some(Segments {
        preamble: s[..start_idx].to_string(),
        auto_body: s[body_start..end_abs].to_string(),
        postamble: s[postamble_start..].to_string(),
    })
}

#[cfg(test)]
mod tests;
