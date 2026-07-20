//! Selection gate: the real gate for the harvest source (Gate-0 domain
//! blocklist is a structural no-op for sessions, design doc Architecture).
//! Follows the documented gate shape - `fn -> Result<(), RejectionRecord>` -
//! exactly like `stages::raw::run_gate_1`/`run_gate_2`, so a rejected session
//! is forensically inspectable via its `rejection.yml`.
//!
//! Signals (all config-tunable via `HarvestConfig`, design doc Selection):
//! 1. `dormant == true` (never harvest a session mid-flight)
//! 2. `enrich-status == ok` (enrichment does NOT imply dormancy; both required)
//! 3. cwd is a real repo (clyde's `repo` field is present, well-formed
//!    `<org>/<repo>`)
//! 4. `n-msgs >= min_msgs` (substantive, not a one-shot)
//! 5. title/first-prompt match no exclusion pattern

use chrono::{DateTime, Utc};
use regex::Regex;

use super::contract::{EnrichStatus, SessionRecord};
use crate::types::{GateId, RejectionRecord, StageKind};

/// Compiled selection criteria. Regexes are compiled ONCE by the orchestrator
/// (never per-record) so the per-session loop stays cheap.
#[derive(Debug)]
pub struct SelectionConfig {
    pub min_msgs: i64,
    pub exclude_patterns: Vec<Regex>,
}

impl SelectionConfig {
    /// Compile `HarvestConfig.exclude_patterns` into regexes. A malformed
    /// pattern is a loud config error, not a silently-dropped rule.
    pub fn compile(min_msgs: usize, patterns: &[String]) -> eyre::Result<Self> {
        log::debug!(
            "harvest::SelectionConfig::compile: min_msgs={min_msgs} pattern_count={}",
            patterns.len()
        );
        let mut compiled = Vec::with_capacity(patterns.len());
        for p in patterns {
            let re = Regex::new(p).map_err(|e| eyre::eyre!("harvest.exclude-patterns: invalid regex {p:?}: {e}"))?;
            compiled.push(re);
        }
        Ok(Self {
            min_msgs: min_msgs as i64,
            exclude_patterns: compiled,
        })
    }
}

/// A well-formed `<org>/<repo>` is exactly one `/` splitting two non-empty
/// components. Mirrors the Phase 9 validator's shape check (kept local so the
/// gate has no cross-phase dependency).
fn is_repo_slug(repo: &str) -> bool {
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(org), Some(name), None) => !org.is_empty() && !name.is_empty(),
        _ => false,
    }
}

/// Build the rejection record for a declined session. `trace_id` is generated
/// at selection time by the orchestrator (before any body fetch) so the reject
/// has a receipts key and a `rejection.yml` home.
fn reject(record: &SessionRecord, trace_id: &str, reason: String) -> Box<RejectionRecord> {
    log::debug!(
        "harvest::select::reject: trace={trace_id} session={} reason={reason}",
        record.session_id
    );
    Box::new(RejectionRecord {
        trace: trace_id.to_string(),
        stage: StageKind::Raw,
        gate: GateId::Selection,
        reason,
        rejected_at: Utc::now().to_rfc3339(),
        raw_artifact: None,
        source: Some(record.clyde_uri()),
        domain: None,
        blocklist_updated: false,
        retriable_after: None,
    })
}

/// The selection gate. `Ok(())` = the session earns a note; `Err(record)` =
/// declined, with a specific reason. First failing signal wins (dormancy and
/// enrichment first, since a mid-flight or unenriched session is never
/// harvestable regardless of size).
pub fn evaluate_selection(
    record: &SessionRecord,
    config: &SelectionConfig,
    trace_id: &str,
) -> Result<(), Box<RejectionRecord>> {
    log::debug!(
        "harvest::select::evaluate_selection: trace={trace_id} session={} dormant={} enrich={:?} n_msgs={} repo={:?}",
        record.session_id,
        record.dormant,
        record.enrich_status,
        record.n_msgs,
        record.repo
    );

    if !record.dormant {
        return Err(reject(
            record,
            trace_id,
            "session is not dormant (still in flight)".to_string(),
        ));
    }

    match record.enrich_status {
        Some(EnrichStatus::Ok) => {}
        other => {
            let label = match other {
                Some(EnrichStatus::SkippedPersonal) => "skipped-personal",
                Some(EnrichStatus::SkippedEmpty) => "skipped-empty",
                Some(EnrichStatus::Failed) => "failed",
                Some(EnrichStatus::Ok) => unreachable!(),
                None => "null",
            };
            return Err(reject(record, trace_id, format!("enrich-status is {label}, not ok")));
        }
    }

    match record.repo.as_deref() {
        Some(repo) if is_repo_slug(repo) => {}
        Some(repo) => {
            return Err(reject(
                record,
                trace_id,
                format!("cwd repo {repo:?} is not a well-formed <org>/<repo>"),
            ));
        }
        None => {
            return Err(reject(
                record,
                trace_id,
                format!(
                    "cwd {:?} is not a repo (no <org>/<repo> anchor)",
                    record.cwd.as_deref().unwrap_or("<null>")
                ),
            ));
        }
    }

    if record.n_msgs < config.min_msgs {
        return Err(reject(
            record,
            trace_id,
            format!("below message threshold: {} < {}", record.n_msgs, config.min_msgs),
        ));
    }

    // A null/unparseable `created` is rejected HERE, at the selection stage,
    // so it never reaches `cluster::parse_ts` (which errors the WHOLE plan on
    // an absent/unparseable created). `modified` stays non-null in the
    // contract, so only `created` needs this guard (harvest-completion design,
    // Data Model). `None` matches no exclusion pattern below, so this guard
    // runs first.
    match record.created.as_deref() {
        Some(created) if DateTime::parse_from_rfc3339(created).is_ok() => {}
        Some(created) => {
            return Err(reject(
                record,
                trace_id,
                format!("created timestamp {created:?} is not valid RFC-3339"),
            ));
        }
        None => {
            return Err(reject(
                record,
                trace_id,
                "created timestamp is null (empty/never-touched session)".to_string(),
            ));
        }
    }

    // A `None` title/first-prompt matches no pattern (an absent field cannot be
    // excluded); `.as_deref().unwrap_or("")` keeps the non-null behavior
    // byte-identical.
    for re in &config.exclude_patterns {
        if re.is_match(record.title.as_deref().unwrap_or(""))
            || re.is_match(record.first_prompt.as_deref().unwrap_or(""))
        {
            return Err(reject(
                record,
                trace_id,
                format!("excluded by pattern {:?}", re.as_str()),
            ));
        }
    }

    log::debug!(
        "harvest::select::evaluate_selection: SELECTED session={} repo={:?}",
        record.session_id,
        record.repo
    );
    Ok(())
}

#[cfg(test)]
mod tests;
