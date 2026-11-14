//! Doctor check: the topic map's status, age, coverage and drift
//! (`docs/plan/2026-11-14-topic-map.md`).

use vault::search::{SearchIndex, TopicPeek, TopicStatus};

use super::Finding;

const TRAIN_FIX: &str = "sb cortex topics --train";

/// The `topics` section. Reads the search DB through a read-only connection
/// (`SearchIndex::peek_topics`), so it neither migrates nor creates it. The
/// exclude list is the one cortex's tick uses; a cortex config that does not
/// load is the config section's finding, so the default list stands in here.
pub(super) fn findings() -> Vec<Finding> {
    let exclude_types = cortex::config::Config::load_service_config(&vault::paths::cortex_config())
        .map(|cfg| cfg.topics.exclude_types)
        .unwrap_or_else(|_| cortex::config::TopicsConfig::default().exclude_types);
    let peek = SearchIndex::peek_topics(&vault::paths::oracle_db_path(), &exclude_types);
    findings_for(peek, chrono::Utc::now().timestamp())
}

fn findings_for(peek: eyre::Result<Option<TopicPeek>>, now: i64) -> Vec<Finding> {
    log::debug!("topics::findings_for: peek={peek:?} now={now}");
    let peek = match peek {
        Ok(Some(peek)) => peek,
        Ok(None) => return vec![Finding::info("no search DB or no embedding pin yet; no topic map")],
        Err(e) => {
            return vec![Finding::error(
                format!("topic state unreadable: {e}"),
                "sb cortex topics --clear, then sb cortex topics --train",
            )];
        }
    };
    let Some(meta) = &peek.meta else {
        return vec![Finding::info(format!(
            "status: absent (no generation; `{TRAIN_FIX}` builds one)"
        ))];
    };
    let age = age_text(now - meta.trained_at);
    let summary = format!(
        "generation {} trained {age} ago, k={}, assigned {} / in-scope {}",
        meta.generation, meta.k, peek.assigned, peek.in_scope
    );
    if peek.status == TopicStatus::Stale {
        return vec![Finding::warn(
            format!(
                "status: stale, {summary}; trained for {} dim {}, which the index no longer pins",
                meta.model_version, meta.dim
            ),
            TRAIN_FIX,
        )];
    }
    let drift = match peek.since_train_mean_cosine {
        Some(mean) => format!(
            "mean cosine {mean:.3} over {} note(s) assigned since the train, train-mean-cosine {:.3}",
            peek.assigned_since_train, meta.train_mean_cosine
        ),
        None => format!(
            "no notes assigned since the train, train-mean-cosine {:.3}",
            meta.train_mean_cosine
        ),
    };
    vec![Finding::ok(format!("status: valid, {summary}; {drift}"))]
}

fn age_text(secs: i64) -> String {
    match secs.max(0) {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s => format!("{}m", s / 60),
    }
}

#[cfg(test)]
mod tests;
