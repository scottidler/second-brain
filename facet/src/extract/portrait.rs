//! Portrait rollups: Opus-tier cross-work-item synthesis per mode.
//!
//! Input is the already-mined `judgment_moments` rows for one mode
//! over the last `portrait.window_days` days, capped at
//! `portrait.max_moments_per_mode`. Output is one
//! `notes/facet/portraits/<mode>.md` portrait note with a fencepost-
//! merged body so operator-added marginalia survives re-renders.

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::extract::JudgmentMoment;
use crate::fabric::{FabricCaller, request};
use crate::ledger::Ledger;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PortraitOutput {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub moments_cited: Vec<MomentCitation>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MomentCitation {
    pub workitem_slug: String,
    pub short_description: String,
}

/// Synthesise one portrait note for `mode` over the recent window.
/// Returns the rendered Markdown body. Empty/single-moment inputs are
/// surfaced via the LLM's own `title == ""` convention; the caller
/// skips rendering in that case.
pub async fn portrait_for_mode(
    mode: &str,
    config: &Config,
    ledger: &Ledger,
    fabric: &dyn FabricCaller,
) -> Result<Option<String>> {
    log::debug!("portrait_for_mode: mode={mode}");
    let moments = ledger.moments_by_mode(
        mode,
        config.portrait.window_days,
        config.portrait.max_moments_per_mode as u32,
    )?;
    if moments.len() < 2 {
        log::info!(
            "portrait_for_mode: skipping {mode} (have {} moments, need >= 2)",
            moments.len()
        );
        return Ok(None);
    }
    let digest = build_digest(mode, &moments, ledger)?;
    let req = request(
        "facet-portrait",
        digest,
        &config.llm.portrait_model,
        config.llm.timeout_secs,
    );
    let raw = fabric.call(req).await.context("portrait LLM call")?;
    let parsed: PortraitOutput =
        serde_yaml::from_str(&raw).with_context(|| format!("parse portrait YAML (got {} bytes)", raw.len()))?;
    if parsed.title.is_empty() && parsed.body.is_empty() {
        return Ok(None);
    }
    Ok(Some(render_portrait_note(mode, &parsed)))
}

fn build_digest(mode: &str, moments: &[JudgmentMoment], ledger: &Ledger) -> Result<String> {
    let mut s = String::new();
    s.push_str(&format!("mode: {}\n", yaml_str(mode)));
    s.push_str("moments:\n");
    for m in moments {
        let slug = ledger
            .workitem_by_id(m.workitem_id)?
            .map(|w| w.slug)
            .unwrap_or_else(|| "unknown".to_string());
        let title = ledger
            .workitem_by_id(m.workitem_id)?
            .map(|w| w.title)
            .unwrap_or_default();
        s.push_str(&format!("  - workitem_slug: {}\n", yaml_str(&slug)));
        s.push_str(&format!("    workitem_title: {}\n", yaml_str(&title)));
        s.push_str(&format!("    ai_move: {}\n", yaml_str(&m.ai_move)));
        s.push_str(&format!("    scott_move: {}\n", yaml_str(&m.scott_move)));
        s.push_str(&format!("    quote_excerpt: {}\n", yaml_str(&m.quote_excerpt)));
        s.push_str(&format!("    why_it_matters: {}\n", yaml_str(&m.why_it_matters)));
    }
    Ok(s)
}

fn yaml_str(s: &str) -> String {
    serde_yaml::to_string(s)
        .unwrap_or_else(|_| format!("{s:?}"))
        .trim_end()
        .to_string()
}

fn render_portrait_note(mode: &str, p: &PortraitOutput) -> String {
    let mut s = String::new();
    s.push_str("<!-- facet:auto:begin frontmatter -->\n");
    s.push_str("---\n");
    s.push_str(&format!("title: {}\n", serde_yaml_value(&p.title)));
    s.push_str(&format!("date: {}\n", chrono::Utc::now().format("%Y-%m-%d")));
    s.push_str("type: facet-portrait\n");
    s.push_str("origin: assisted\n");
    s.push_str("method: facet\n");
    s.push_str(&format!("facet-mode: {mode}\n"));
    s.push_str(&format!("facet-moments-included: {}\n", p.moments_cited.len()));
    s.push_str("facet-extractor: facet-v1\n");
    s.push_str("tags:\n  - facet\n  - portrait\n");
    s.push_str(&format!("  - {mode}\n"));
    s.push_str("---\n");
    s.push_str("<!-- facet:auto:end frontmatter -->\n\n");

    s.push_str("<!-- facet:auto:begin body -->\n");
    s.push_str(&format!("# {}\n\n", p.title));
    s.push_str(&p.body);
    if !p.body.ends_with('\n') {
        s.push('\n');
    }
    s.push('\n');
    if !p.moments_cited.is_empty() {
        s.push_str("## Representative moments\n\n");
        for c in &p.moments_cited {
            s.push_str(&format!(
                "- [[work-items/{}]] - {}\n",
                c.workitem_slug, c.short_description
            ));
        }
        s.push('\n');
    }
    s.push_str("<!-- facet:auto:end body -->\n");
    s
}

fn serde_yaml_value(s: &str) -> String {
    serde_yaml::to_string(s)
        .unwrap_or_else(|_| format!("{s:?}"))
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests;
