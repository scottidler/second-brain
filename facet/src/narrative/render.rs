//! Spectrum (narrative) note renderer.
//!
//! One file per narrative under `notes/facet/spectra/<slug>.md`.
//! Frontmatter carries the v2 archetype + status + cluster-key + gem
//! citations so the next narrate pass can read existing notes to
//! honour operator rejections (see [`crate::narrative::SpectrumStatus`]).
//!
//! Phase 5 of the v2 redesign.

use std::path::Path;

use eyre::{Context, Result};

use crate::narrative::{Archetype, Narrative, SpectrumStatus};
use crate::render::{block, write_atomic};

#[cfg(test)]
mod tests;

/// Render and write the spectrum note for `narrative`. Reads any
/// pre-existing file to preserve operator-owned content outside
/// fenceposts AND to surface the operator's `facet-spectrum-status`
/// edit (see [`read_spectrum_status`] for the reader path).
pub fn render_spectrum_note(
    target_path: &Path,
    narrative: &Narrative,
    archetype: Archetype,
    cluster_key: &str,
) -> Result<()> {
    log::debug!(
        "render_spectrum_note: target={} narrative_id={} archetype={} cluster_key={}",
        target_path.display(),
        narrative.id,
        archetype.as_str(),
        cluster_key,
    );
    let existing = if target_path.exists() {
        Some(
            std::fs::read_to_string(target_path)
                .with_context(|| format!("read existing spectrum note at {}", target_path.display()))?,
        )
    } else {
        None
    };
    let body = render_to_string(narrative, archetype, cluster_key, existing.as_deref());
    write_atomic(target_path, &body)
}

/// Pure render. Public for golden tests.
pub fn render_to_string(
    narrative: &Narrative,
    archetype: Archetype,
    cluster_key: &str,
    existing: Option<&str>,
) -> String {
    let fresh = build_fresh_template(narrative, archetype, cluster_key);
    match existing {
        None => fresh,
        Some(existing) => block::merge(existing, &fresh),
    }
}

fn build_fresh_template(narrative: &Narrative, archetype: Archetype, cluster_key: &str) -> String {
    let mut s = String::new();
    s.push_str("<!-- facet:auto:begin frontmatter -->\n");
    s.push_str(&render_frontmatter(narrative, archetype, cluster_key));
    s.push_str("<!-- facet:auto:end frontmatter -->\n\n");

    s.push_str("<!-- facet:auto:begin header -->\n");
    s.push_str(&render_header(narrative));
    s.push_str("<!-- facet:auto:end header -->\n\n");

    s.push_str("<!-- facet:auto:begin body -->\n");
    s.push_str(&narrative.body_md);
    if !narrative.body_md.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("\n<!-- facet:auto:end body -->\n\n");

    s.push_str("<!-- facet:auto:begin citations -->\n");
    s.push_str(&render_citations(narrative));
    s.push_str("<!-- facet:auto:end citations -->\n\n");

    s.push_str("<!-- facet:auto:begin footer -->\n");
    s.push_str(&render_footer(narrative));
    s.push_str("<!-- facet:auto:end footer -->\n");
    s
}

fn render_frontmatter(narrative: &Narrative, archetype: Archetype, cluster_key: &str) -> String {
    let mut m = serde_yaml::Mapping::new();
    let insert_str = |m: &mut serde_yaml::Mapping, k: &str, v: String| {
        m.insert(serde_yaml::Value::String(k.into()), serde_yaml::Value::String(v));
    };
    insert_str(&mut m, "title", narrative.title.clone());
    insert_str(&mut m, "date", narrative.synthesised_at.format("%Y-%m-%d").to_string());
    insert_str(&mut m, "type", "facet-spectrum".to_string());
    insert_str(&mut m, "origin", "assisted".to_string());
    insert_str(&mut m, "method", "facet".to_string());
    insert_str(&mut m, "status", "unread".to_string());
    insert_str(&mut m, "domain", "ai".to_string());
    insert_str(&mut m, "facet-slug", narrative.slug.clone());
    // Operator-editable status. Default to active on first render; the
    // operator flips to "rejected" by hand to suppress regeneration.
    insert_str(
        &mut m,
        "facet-spectrum-status",
        SpectrumStatus::Active.as_str().to_string(),
    );
    insert_str(&mut m, "facet-spectrum-archetype", archetype.as_str().to_string());
    insert_str(&mut m, "facet-spectrum-cluster-key", cluster_key.to_string());
    m.insert(
        serde_yaml::Value::String("facet-spectrum-gem-ids".into()),
        serde_yaml::Value::Sequence(
            narrative
                .gem_ids
                .iter()
                .map(|&id| serde_yaml::Value::Number(id.into()))
                .collect(),
        ),
    );
    insert_str(&mut m, "facet-synthesiser-model", narrative.synthesiser_model.clone());
    m.insert(
        serde_yaml::Value::String("facet-revision".into()),
        serde_yaml::Value::Number(narrative.revision.into()),
    );
    m.insert(
        serde_yaml::Value::String("tags".into()),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("facet".into()),
            serde_yaml::Value::String("spectrum".into()),
        ]),
    );
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(m))
        .unwrap_or_else(|_| String::from("# yaml render failed\n"));
    format!("---\n{yaml}---\n")
}

fn render_header(narrative: &Narrative) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", narrative.title));
    s.push_str(&format!("> {}\n\n", narrative.thesis));
    s.push_str("---\n\n");
    s
}

fn render_citations(narrative: &Narrative) -> String {
    let mut s = String::new();
    s.push_str("## Cited Gems\n\n");
    if narrative.gem_ids.is_empty() {
        s.push_str("*(no gems cited)*\n\n");
        return s;
    }
    for id in &narrative.gem_ids {
        s.push_str(&format!("- gem #{id}\n"));
    }
    s.push('\n');
    s
}

fn render_footer(narrative: &Narrative) -> String {
    format!(
        "---\n\n*Synthesised by `sb facet narrate`. To re-synthesise: `sb facet narrate`. \
         Mark `facet-spectrum-status: rejected` above to suppress this spectrum on next pass.*\n\n\
         *Model: {}, revision {}.*\n",
        narrative.synthesiser_model, narrative.revision,
    )
}

/// Parse the `facet-spectrum-status`, `facet-spectrum-cluster-key`, and
/// `facet-spectrum-gem-ids` keys from an existing spectrum file's
/// frontmatter. Used by the rejection-suppression path: when a new
/// cluster has >= 80% gem-id overlap with an existing rejected
/// spectrum, the narrate pass skips it.
///
/// Returns `Ok(None)` if the file doesn't exist or has no
/// recognisable frontmatter.
pub fn read_spectrum_meta(path: &Path) -> Result<Option<SpectrumMeta>> {
    if !path.exists() {
        return Ok(None);
    }
    let body = std::fs::read_to_string(path).with_context(|| format!("read spectrum note {}", path.display()))?;
    Ok(parse_meta_from_body(&body))
}

#[derive(Debug, Clone)]
pub struct SpectrumMeta {
    pub status: SpectrumStatus,
    pub cluster_key: String,
    pub gem_ids: Vec<i64>,
    pub archetype: Option<Archetype>,
}

fn parse_meta_from_body(body: &str) -> Option<SpectrumMeta> {
    let yaml = extract_frontmatter_yaml(body)?;
    let mapping: serde_yaml::Mapping = serde_yaml::from_str(&yaml).ok()?;
    let status = mapping
        .get(serde_yaml::Value::String("facet-spectrum-status".into()))
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "rejected" => Some(SpectrumStatus::Rejected),
            "active" => Some(SpectrumStatus::Active),
            _ => None,
        })
        .unwrap_or(SpectrumStatus::Active);
    let cluster_key = mapping
        .get(serde_yaml::Value::String("facet-spectrum-cluster-key".into()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let gem_ids: Vec<i64> = mapping
        .get(serde_yaml::Value::String("facet-spectrum-gem-ids".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| seq.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let archetype = mapping
        .get(serde_yaml::Value::String("facet-spectrum-archetype".into()))
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "session" => Some(Archetype::Session),
            "cross-session" => Some(Archetype::CrossSession),
            "evergreen" => Some(Archetype::Evergreen),
            _ => None,
        });
    Some(SpectrumMeta {
        status,
        cluster_key,
        gem_ids,
        archetype,
    })
}

fn extract_frontmatter_yaml(body: &str) -> Option<String> {
    // The renderer wraps the frontmatter in a fencepost, so the
    // content between `---` / `---` is the YAML we want. Scan after
    // the fencepost-begin marker for resilience.
    let after_begin = body
        .find("<!-- facet:auto:begin frontmatter -->")
        .map(|idx| &body[idx..])
        .unwrap_or(body);
    let opener = after_begin.find("---")?;
    let after_opener = &after_begin[opener + 3..];
    // Skip newline after the opening ---.
    let after_opener = after_opener.trim_start_matches('\n');
    let closer_rel = after_opener.find("\n---")?;
    Some(after_opener[..closer_rel].to_string())
}
