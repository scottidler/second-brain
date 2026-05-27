//! Per-tick harvest: scan -> cluster -> extract -> render.
//!
//! Concurrency caps come from [`crate::config::ConcurrencyConfig`].
//! Per-session and per-(session, cluster_assignment) failures are
//! contained: one session's bad LLM call does not skip the others.

use std::path::Path;
use std::sync::Arc;

use eyre::{Context, Result};

use super::TickReport;
use crate::config::Config;
use crate::extract::mine::mine_moments;
use crate::extract::portrait::portrait_for_mode;
use crate::fabric::{FabricCaller, FabricShell};
use crate::jsonl::Turn;
use crate::ledger::Ledger;
use crate::render::render_work_item_note;
use crate::scan::{FacetSession, enumerate};
use crate::workitem::cluster::cluster_new_turns;

/// Drive one tick end-to-end with a production fabric caller. The
/// `vault_root` is the resolved vault path; notes are written under
/// `<vault_root>/<config.vault.workitems_dir>/<slug>.md`.
pub async fn run_once(config: &Config, ledger: &Ledger, vault_root: &Path) -> Result<TickReport> {
    log::info!("facet::harvest::run_once: vault_root={}", vault_root.display());
    let fabric: Arc<dyn FabricCaller> = Arc::new(FabricShell::new(config.llm.fabric_binary.clone()));
    run_with_fabric(config, ledger, vault_root, fabric.as_ref()).await
}

/// Generic over the fabric caller so integration tests can swap in a
/// `FakeFabric`.
pub async fn run_with_fabric(
    config: &Config,
    ledger: &Ledger,
    vault_root: &Path,
    fabric: &dyn FabricCaller,
) -> Result<TickReport> {
    let mut report = TickReport::default();

    // 1. Scan.
    let sessions = enumerate(config, ledger)?;
    report.sessions_seen = sessions.len();

    // 2. Cluster, bounded by max_sessions_per_tick.
    let cap = config.concurrency.max_sessions_per_tick.max(1);
    let deferred = sessions.len().saturating_sub(cap);
    if deferred > 0 {
        // The per-tick cap is our v1 stand-in for budget enforcement (the
        // real budget caps are a known gap; see Architect round 1). Fire
        // the notification anyway so operators see when ticks are
        // shedding load.
        crate::notify::on_budget_exhausted(
            &config.notify,
            &format!("max-sessions-per-tick={cap} reached; deferring {deferred} session(s) to next tick"),
        );
    }
    let mut touched_workitems: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    for session in sessions.iter().take(cap) {
        match cluster_new_turns(session, config, ledger, fabric).await {
            Ok(assignments) => {
                report.cluster_assignments_created += assignments.len();
                // Track touched work-items for the final render pass.
                if let Some(ws) = lookup_session_workitems(ledger, &session.session_uuid)? {
                    for id in ws {
                        touched_workitems.insert(id);
                    }
                }
            }
            Err(e) => {
                report.failures += 1;
                log::warn!(
                    "facet::harvest: cluster failed for session {}: {e:#}",
                    session.session_uuid
                );
                ledger.record_session_failure(&session.session_uuid, "cluster", &format!("{e:#}"))?;
            }
        }
    }

    // 3. Extract every pending cluster_assignments row up to the
    //    per-tick session cap. Each row failure is local.
    let pending = ledger.pending_cluster_assignments(cap as u32 * 4)?;
    for row in pending {
        let workitem = match ledger.workitem_by_id(row.workitem_id)? {
            Some(w) => w,
            None => {
                log::warn!(
                    "facet::harvest: cluster row {} references missing workitem {}",
                    row.id,
                    row.workitem_id
                );
                continue;
            }
        };
        let session_row = match ledger.get_session(&row.session_uuid)? {
            Some(s) => s,
            None => {
                log::warn!(
                    "facet::harvest: cluster row {} references missing session {}",
                    row.id,
                    row.session_uuid
                );
                continue;
            }
        };
        let turns = match read_turn_slice(config, &row, &session_row, &sessions) {
            Ok(t) => t,
            Err(e) => {
                report.failures += 1;
                log::warn!(
                    "facet::harvest: could not read turn slice for cluster row {}: {e:#}",
                    row.id
                );
                continue;
            }
        };
        match mine_moments(
            &row,
            &turns,
            &workitem.slug,
            &workitem.title,
            workitem.repos.first().map(|s| s.as_str()),
            config,
            ledger,
            fabric,
        )
        .await
        {
            Ok(out) => {
                report.moments_extracted += out.len();
                touched_workitems.insert(workitem.id);
            }
            Err(e) => {
                report.failures += 1;
                log::warn!(
                    "facet::harvest: extract failed for cluster row {} workitem {}: {e:#}",
                    row.id,
                    workitem.id
                );
                ledger.record_session_failure(&row.session_uuid, "extract", &format!("{e:#}"))?;
            }
        }
    }

    // 4. Render every touched work-item.
    for id in touched_workitems {
        let Some(w) = ledger.workitem_by_id(id)? else { continue };
        let moments = ledger.moments_for_workitem(id)?;
        let target = vault_root
            .join(&config.vault.workitems_dir)
            .join(format!("{}.md", w.slug));
        if let Err(e) = render_work_item_note(&target, &w, &moments) {
            report.failures += 1;
            log::warn!(
                "facet::harvest: render failed for workitem {} ({}): {e:#}",
                w.id,
                w.slug
            );
        } else {
            report.workitems_rendered += 1;
        }
    }

    // 5. Dormancy sweep.
    let _flipped = ledger.mark_dormant(chrono::Utc::now(), config.dormancy.inactive_days)?;
    ledger.meta_set("last-harvest-tick", &chrono::Utc::now().to_rfc3339())?;

    Ok(report)
}

/// Run the portrait rollup over every distinct mode that has at least
/// two moments in the configured window. Writes one
/// `<vault_root>/<config.vault.portraits_dir>/<mode>.md` per portrait
/// that was actually synthesised; the rest are skipped silently per
/// the LLM contract (empty title => skip).
pub async fn run_portrait_rollup(config: &Config, ledger: &Ledger, vault_root: &Path) -> Result<usize> {
    let fabric: Arc<dyn FabricCaller> = Arc::new(FabricShell::new(config.llm.fabric_binary.clone()));
    run_portrait_rollup_with_fabric(config, ledger, vault_root, fabric.as_ref()).await
}

pub async fn run_portrait_rollup_with_fabric(
    config: &Config,
    ledger: &Ledger,
    vault_root: &Path,
    fabric: &dyn FabricCaller,
) -> Result<usize> {
    let modes = list_modes(ledger)?;
    let mut written = 0usize;
    for mode in modes {
        match portrait_for_mode(&mode, config, ledger, fabric).await {
            Ok(Some(body)) => {
                let target = vault_root.join(&config.vault.portraits_dir).join(format!("{mode}.md"));
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let merged = match std::fs::read_to_string(&target) {
                    Ok(existing) => crate::render::block::merge(&existing, &body),
                    Err(_) => body,
                };
                std::fs::write(&target, merged).context("write portrait note")?;
                written += 1;
            }
            Ok(None) => {}
            Err(e) => log::warn!("portrait rollup failed for mode {mode}: {e:#}"),
        }
    }
    ledger.meta_set("last-portrait-tick", &chrono::Utc::now().to_rfc3339())?;
    Ok(written)
}

fn list_modes(ledger: &Ledger) -> Result<Vec<String>> {
    ledger.with_conn(|c| {
        let mut stmt = c.prepare("SELECT DISTINCT mode FROM judgment_moments ORDER BY mode")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

fn read_turn_slice(
    _config: &Config,
    row: &crate::ledger::clusters::ClusterAssignmentRow,
    session_row: &crate::ledger::sessions::SessionRow,
    sessions: &[FacetSession],
) -> Result<Vec<Turn>> {
    // Fast path: the in-memory FacetSession from the same tick already
    // carries the new turns. If extract is running on a row that was
    // clustered in a previous tick, fall back to re-parsing the JSONL
    // from offset 0 and taking the [first_turn_uuid, last_turn_uuid]
    // slice.
    if let Some(s) = sessions.iter().find(|s| s.session_uuid == row.session_uuid)
        && let Some(slice) = bound_slice(&s.parsed.turns, &row.first_turn_uuid, &row.last_turn_uuid)
    {
        return Ok(slice);
    }
    let path = jsonl_path_for(session_row)?;
    let parsed = crate::jsonl::parse_session_file(&path, 0).map_err(|e| eyre::eyre!("re-parse for extract: {e}"))?;
    bound_slice(&parsed.turns, &row.first_turn_uuid, &row.last_turn_uuid)
        .ok_or_else(|| eyre::eyre!("turn range not found in session file"))
}

/// Reconstruct the JSONL path from a session row. The encoded-cwd
/// directory follows Claude Code's convention: leading `-` plus every
/// path separator replaced with `-`.
fn jsonl_path_for(session_row: &crate::ledger::sessions::SessionRow) -> Result<std::path::PathBuf> {
    let projects = crate::config::Config::default().claude_projects_root;
    let encoded = format!("-{}", session_row.cwd.trim_start_matches('/').replace('/', "-"));
    Ok(projects
        .join(encoded)
        .join(format!("{}.jsonl", session_row.session_uuid)))
}

fn bound_slice(turns: &[Turn], first_uuid: &str, last_uuid: &str) -> Option<Vec<Turn>> {
    let start = turns.iter().position(|t| t.uuid == first_uuid)?;
    let end = turns.iter().position(|t| t.uuid == last_uuid)?;
    if end < start {
        return None;
    }
    Some(turns[start..=end].to_vec())
}

fn lookup_session_workitems(ledger: &Ledger, session_uuid: &str) -> Result<Option<Vec<i64>>> {
    ledger.with_conn(|c| {
        let mut stmt = c.prepare("SELECT workitem_id FROM session_workitem WHERE session_uuid = ?1")?;
        let rows = stmt.query_map(rusqlite::params![session_uuid], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        if out.is_empty() { Ok(None) } else { Ok(Some(out)) }
    })
}
