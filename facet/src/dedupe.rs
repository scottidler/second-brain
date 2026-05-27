//! Slug-suffix deduplication.
//!
//! The cluster LLM occasionally invents a new slug for a concept that
//! already has a work-item, suffixing with `-2`, `-3`, etc. (cross-repo
//! visibility from `known_workitems` reduces but does not eliminate
//! this). This module detects those mechanical duplicates and merges
//! them: judgment moments, cluster assignments, session links, and
//! repo links from the suffixed work-item are re-pointed at the base
//! work-item; the suffixed work-item is then deleted.
//!
//! Vault-side cleanup (rkvr-archive the duplicate's prism note, then
//! re-render the base) is the CLI wrapper's responsibility - this
//! module is pure ledger surgery.

use eyre::{Context, Result};

use crate::ledger::Ledger;

/// A planned merge: every workitem in `duplicates` will be folded into
/// `base`. The slugs are surfaced for logging; ids are what the SQL
/// statements actually touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    pub base_id: i64,
    pub base_slug: String,
    pub duplicate_id: i64,
    pub duplicate_slug: String,
}

/// Summary of one executed merge.
#[derive(Debug, Default, Clone)]
pub struct MergeReport {
    pub moments_moved: usize,
    pub moments_collided: usize,
    pub cluster_rows_moved: usize,
    pub cluster_rows_collided: usize,
    pub session_links_moved: usize,
    pub session_links_collided: usize,
    pub repo_links_moved: usize,
    pub repo_links_collided: usize,
}

/// Walk every work-item looking for slugs of the form `<base>-<digits>`.
/// Returns a plan for each match where `<base>` also exists.
pub fn plan_merges(ledger: &Ledger) -> Result<Vec<MergePlan>> {
    let all = list_id_slug(ledger)?;
    let by_slug: std::collections::HashMap<String, i64> = all.iter().map(|(id, slug)| (slug.clone(), *id)).collect();
    let mut out = Vec::new();
    for (id, slug) in &all {
        let Some(base_slug) = strip_dup_suffix(slug) else {
            continue;
        };
        let Some(base_id) = by_slug.get(base_slug) else {
            continue;
        };
        if *base_id == *id {
            continue;
        }
        out.push(MergePlan {
            base_id: *base_id,
            base_slug: base_slug.to_string(),
            duplicate_id: *id,
            duplicate_slug: slug.clone(),
        });
    }
    Ok(out)
}

/// Execute one merge as a single transaction. Idempotent: re-running
/// against an already-merged pair is a no-op (the dup row is gone).
pub fn execute(ledger: &Ledger, plan: &MergePlan) -> Result<MergeReport> {
    log::info!(
        "facet::dedupe::execute: merging dup={}({}) -> base={}({})",
        plan.duplicate_id,
        plan.duplicate_slug,
        plan.base_id,
        plan.base_slug
    );
    let report = ledger.with_conn(|c| {
        let tx = c.transaction().context("begin merge tx")?;
        let mut r = MergeReport::default();

        for spec in &MOVE_SPECS {
            let before = count_rows(&tx, spec.table, "workitem_id", plan.duplicate_id)?;
            let moved = tx
                .execute(
                    &format!(
                        "UPDATE OR IGNORE {} SET workitem_id = ?1 WHERE workitem_id = ?2",
                        spec.table
                    ),
                    rusqlite::params![plan.base_id, plan.duplicate_id],
                )
                .with_context(|| format!("merge update {}", spec.table))? as usize;
            let leftover = tx
                .execute(
                    &format!("DELETE FROM {} WHERE workitem_id = ?1", spec.table),
                    rusqlite::params![plan.duplicate_id],
                )
                .with_context(|| format!("merge cleanup {}", spec.table))? as usize;
            let collided = (before.saturating_sub(moved)).max(leftover);
            (spec.assign_moved)(&mut r, moved);
            (spec.assign_collided)(&mut r, collided);
        }

        tx.execute(
            "DELETE FROM work_items WHERE id = ?1",
            rusqlite::params![plan.duplicate_id],
        )
        .context("delete duplicate work_item")?;

        tx.commit().context("commit merge")?;
        Ok(r)
    })?;
    Ok(report)
}

struct MoveSpec {
    table: &'static str,
    assign_moved: fn(&mut MergeReport, usize),
    assign_collided: fn(&mut MergeReport, usize),
}

const MOVE_SPECS: [MoveSpec; 4] = [
    MoveSpec {
        table: "judgment_moments",
        assign_moved: |r, n| r.moments_moved = n,
        assign_collided: |r, n| r.moments_collided = n,
    },
    MoveSpec {
        table: "cluster_assignments",
        assign_moved: |r, n| r.cluster_rows_moved = n,
        assign_collided: |r, n| r.cluster_rows_collided = n,
    },
    MoveSpec {
        table: "session_workitem",
        assign_moved: |r, n| r.session_links_moved = n,
        assign_collided: |r, n| r.session_links_collided = n,
    },
    MoveSpec {
        table: "work_item_repos",
        assign_moved: |r, n| r.repo_links_moved = n,
        assign_collided: |r, n| r.repo_links_collided = n,
    },
];

fn count_rows(tx: &rusqlite::Transaction<'_>, table: &str, col: &str, val: i64) -> Result<usize> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {col} = ?1");
    let n: i64 = tx
        .query_row(&sql, rusqlite::params![val], |r| r.get(0))
        .with_context(|| format!("count {table}.{col}"))?;
    Ok(n as usize)
}

fn list_id_slug(ledger: &Ledger) -> Result<Vec<(i64, String)>> {
    ledger.with_conn(|c| {
        let mut stmt = c
            .prepare("SELECT id, slug FROM work_items ORDER BY id")
            .context("prep list_id_slug")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .context("query list_id_slug")?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.context("row list_id_slug")?);
        }
        Ok(out)
    })
}

/// Strip a trailing `-<digits>` from a slug, returning the base slug
/// when present. Returns `None` for slugs that do not end in a
/// hyphen + digits.
pub fn strip_dup_suffix(slug: &str) -> Option<&str> {
    let bytes = slug.as_bytes();
    let mut i = bytes.len();
    while i > 0 && bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == bytes.len() || i == 0 || bytes[i - 1] != b'-' {
        return None;
    }
    let base = &slug[..i - 1];
    if base.is_empty() { None } else { Some(base) }
}

#[cfg(test)]
mod tests;
