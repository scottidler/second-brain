pub use vault::hygiene::{normalize_text_input, sanitize_filename, sanitize_tag};

use crate::config::CanonicalRule;
use url::Url;

/// Resolve a note's filename stem from its title, one layer above
/// `vault::hygiene::sanitize_filename`: a title that sanitizes to empty (e.g.
/// one made entirely of box-drawing/decorative characters `sanitize_filename`
/// strips to nothing) falls back to `untitled-<trace_id>` rather than
/// publishing `inbox/.md`. Trace ids are `vault::trace::generate` output
/// (`{prefix}-{8 hex}`), so the fallback is always a valid, unique-by-trace
/// slug. This is the one seam every note-publish call site goes through.
pub fn note_filename(title: &str, trace_id: &str) -> String {
    let stem = sanitize_filename(title);
    if stem.is_empty() {
        log::debug!("note_filename: title sanitized to empty, falling back to untitled-{trace_id}");
        format!("untitled-{trace_id}")
    } else {
        stem
    }
}

const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "fbclid",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    "msclkid",
    "twclid",
    "li_fat_id",
    "mc_cid",
    "mc_eid",
    "oly_anon_id",
    "oly_enc_id",
    "_openstat",
    "vero_id",
    "wickedid",
    "yclid",
    "hsa_cam",
    "hsa_grp",
    "hsa_mt",
    "hsa_src",
    "hsa_ad",
    "hsa_acc",
    "hsa_net",
    "hsa_ver",
    "hsa_la",
    "hsa_ol",
    "hsa_kw",
    "hsa_tgt",
    "ref",
    "ref_",
    "ref_src",
    "ref_url",
    "feature",
    "si",         // YouTube tracking
    "pp",         // YouTube tracking
    "ab_channel", // YouTube tracking
    // YouTube ephemeral context
    "t",           // timestamp (t=13s, t=1m30s)
    "list",        // playlist ID
    "index",       // playlist position
    "start_radio", // YouTube mix seed
    "flow",        // YouTube flow parameter
    "app",         // app source (app=desktop)
];

pub fn clean_url(raw: &str) -> eyre::Result<String> {
    let mut parsed = Url::parse(raw.trim())?;

    let cleaned_pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| !TRACKING_PARAMS.contains(&key.as_ref()))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if cleaned_pairs.is_empty() {
        parsed.set_query(None);
    } else {
        let query = cleaned_pairs
            .iter()
            .map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") })
            .collect::<Vec<_>>()
            .join("&");
        parsed.set_query(Some(&query));
    }

    // Remove trailing fragment if empty
    if parsed.fragment() == Some("") {
        parsed.set_fragment(None);
    }

    Ok(parsed.to_string())
}

/// Apply config-driven canonicalization rules to a cleaned URL.
/// First matching rule wins. If no rule matches, returns the URL unchanged.
pub fn canonicalize_url(url: &str, rules: &[CanonicalRule]) -> String {
    for rule in rules {
        let re = match regex::Regex::new(&rule.match_regex) {
            Ok(re) => re,
            Err(e) => {
                log::warn!("Invalid canonicalization regex for '{}': {e}", rule.name);
                continue;
            }
        };
        if let Some(caps) = re.captures(url) {
            let mut result = rule.canonical.clone();
            for name in re.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    result = result.replace(&format!("{{{name}}}"), m.as_str());
                }
            }
            return result;
        }
    }
    url.to_string()
}

/// Combined: clean + canonicalize. This is what callers should use.
pub fn normalize_url(raw: &str, rules: &[CanonicalRule]) -> eyre::Result<String> {
    let cleaned = clean_url(raw)?;
    Ok(canonicalize_url(&cleaned, rules))
}

#[cfg(test)]
mod tests;
