//! Dream renderer. One file per dream-finding under
//! `<vault>/<dreams_dir>/`. Markdown only; no ledger write.

use std::path::{Path, PathBuf};

use eyre::{Context, Result};
use sha2::{Digest, Sha256};

use crate::dream::Dream;
use crate::render::write_atomic;

#[cfg(test)]
mod tests;

/// Render all dreams to `dreams_dir`. Returns the list of written
/// paths. Idempotent: each dream's filename is derived from its
/// content so re-running over the same dream set overwrites the same
/// files.
pub fn render_all(dreams: &[Dream], dreams_dir: &Path) -> Result<Vec<PathBuf>> {
    log::debug!(
        "dream::render_all: count={} dest={}",
        dreams.len(),
        dreams_dir.display()
    );
    std::fs::create_dir_all(dreams_dir).with_context(|| format!("mkdir {}", dreams_dir.display()))?;
    let mut written = Vec::with_capacity(dreams.len());
    for d in dreams {
        let filename = dream_filename(d);
        let target = dreams_dir.join(&filename);
        let body = render_one(d);
        if let Err(e) = write_atomic(&target, &body) {
            log::warn!("dream::render_all: write failed for {}: {e:#}", target.display());
            continue;
        }
        written.push(target);
    }
    Ok(written)
}

/// Stable filename per dream: `<kind>-<sha256-12>.md`. Same dream
/// content -> same filename so re-renders overwrite.
pub fn dream_filename(d: &Dream) -> String {
    let kind = match d {
        Dream::SemanticDuplicateGroup { .. } => "semantic-duplicate",
        Dream::CrossReference { .. } => "cross-reference",
        Dream::StaleSpectrum { .. } => "stale-spectrum",
        Dream::NarrativeCandidate { .. } => "narrative-candidate",
    };
    let payload = serde_json::to_string(d).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(payload.as_bytes());
    let hex = hex::encode(h.finalize());
    format!("{kind}-{}.md", &hex[..12])
}

fn render_one(d: &Dream) -> String {
    let mut s = String::new();
    s.push_str("<!-- facet:auto:begin frontmatter -->\n");
    s.push_str(&render_frontmatter(d));
    s.push_str("<!-- facet:auto:end frontmatter -->\n\n");
    s.push_str("<!-- facet:auto:begin body -->\n");
    s.push_str(&render_body(d));
    s.push_str("<!-- facet:auto:end body -->\n");
    s
}

fn render_frontmatter(d: &Dream) -> String {
    let mut m = serde_yaml::Mapping::new();
    let insert = |m: &mut serde_yaml::Mapping, k: &str, v: String| {
        m.insert(serde_yaml::Value::String(k.into()), serde_yaml::Value::String(v));
    };
    insert(&mut m, "title", title_for(d));
    insert(&mut m, "type", "facet-dream".to_string());
    insert(&mut m, "origin", "assisted".to_string());
    insert(&mut m, "method", "facet".to_string());
    insert(&mut m, "status", "unread".to_string());
    insert(&mut m, "domain", "ai".to_string());
    insert(&mut m, "facet-dream-kind", kind_for(d).to_string());
    insert(&mut m, "facet-dream-status", "proposed".to_string());
    m.insert(
        serde_yaml::Value::String("tags".into()),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("facet".into()),
            serde_yaml::Value::String("dream".into()),
        ]),
    );
    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(m))
        .unwrap_or_else(|_| String::from("# yaml render failed\n"));
    format!("---\n{yaml}---\n")
}

fn render_body(d: &Dream) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}\n\n", title_for(d)));
    match d {
        Dream::SemanticDuplicateGroup { gem_ids, canonical } => {
            s.push_str(&format!(
                "Semantic duplicates detected across {} gems; proposed canonical is gem #{canonical}.\n\n",
                gem_ids.len()
            ));
            s.push_str("**Gem ids:**\n\n");
            for id in gem_ids {
                let star = if id == canonical { " (canonical)" } else { "" };
                s.push_str(&format!("- gem #{id}{star}\n"));
            }
        }
        Dream::CrossReference {
            from_gem,
            to_gem,
            relation,
        } => {
            s.push_str(&format!(
                "Gem #{from_gem} appears to reference gem #{to_gem}. Proposed relation: `{relation}`.\n",
            ));
        }
        Dream::StaleSpectrum {
            narrative_id,
            new_gem_ids_since,
        } => {
            s.push_str(&format!(
                "Narrative #{narrative_id} has {} new gem(s) since synthesis.\n\n",
                new_gem_ids_since.len()
            ));
            s.push_str("**New gem ids:**\n\n");
            for id in new_gem_ids_since {
                s.push_str(&format!("- gem #{id}\n"));
            }
        }
        Dream::NarrativeCandidate {
            gem_ids,
            proposed_title,
            proposed_thesis,
        } => {
            s.push_str(&format!("**Proposed title:** {proposed_title}\n\n"));
            s.push_str(&format!("**Proposed thesis:** {proposed_thesis}\n\n"));
            s.push_str(&format!("**Gem ids ({} total):**\n\n", gem_ids.len()));
            for id in gem_ids {
                s.push_str(&format!("- gem #{id}\n"));
            }
        }
    }
    s.push('\n');
    s.push_str("---\n\n");
    s.push_str(
        "*Proposed by `sb facet dream`. NEVER auto-applies; mark \
         `facet-dream-status: accepted` (or `dismissed`) to act on this.*\n",
    );
    s
}

fn title_for(d: &Dream) -> String {
    match d {
        Dream::SemanticDuplicateGroup { gem_ids, .. } => {
            format!("Semantic duplicate group ({} gems)", gem_ids.len())
        }
        Dream::CrossReference { from_gem, to_gem, .. } => format!("Cross-reference gem #{from_gem} -> gem #{to_gem}"),
        Dream::StaleSpectrum {
            narrative_id,
            new_gem_ids_since,
        } => format!(
            "Stale spectrum: narrative #{narrative_id} (+{} new)",
            new_gem_ids_since.len()
        ),
        Dream::NarrativeCandidate { proposed_title, .. } => {
            format!("Narrative candidate: {proposed_title}")
        }
    }
}

fn kind_for(d: &Dream) -> &'static str {
    match d {
        Dream::SemanticDuplicateGroup { .. } => "semantic-duplicate-group",
        Dream::CrossReference { .. } => "cross-reference",
        Dream::StaleSpectrum { .. } => "stale-spectrum",
        Dream::NarrativeCandidate { .. } => "narrative-candidate",
    }
}
