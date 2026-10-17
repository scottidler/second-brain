//! Judge trust: the `judge_trust` table `sb borg eval --inject` writes, and the
//! rule that decides whether a judge check's failures may act.
//!
//! A judge check is trusted when, on every planted class the applicability
//! matrix ([`super::inject::applies`]) marks visible to it:
//! - its paired catch rate is at least `eval.trust.min-paired-catch-rate`,
//! - its applicable sample size is at least `eval.trust.min-samples`, and
//! - the row is for the check's current rubric version.
//!
//! A class the check cannot see never counts against it. Deterministic checks
//! (no model) are not gated: they make no model call. Their rows are still
//! written so the report shows their rates. Whether a check is deterministic
//! comes from the check's name, never from an empty model string: an LLM
//! check with no model ran under fabric's default and is untrusted.

use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, params};

use super::inject::{PlantedClass, applicable_classes};
use super::scores::{DETERMINISTIC_CHECKS, rubric_version_of};
use crate::config::TrustConfig;

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS judge_trust (
    judge_model         TEXT NOT NULL,
    check_name          TEXT NOT NULL,
    planted_class       TEXT NOT NULL,
    rubric_version      TEXT NOT NULL,
    paired_catch_rate   REAL NOT NULL,
    raw_catch_rate      REAL NOT NULL,
    flag_rate           REAL NOT NULL,
    wilson_lower        REAL NOT NULL,
    sample_size         INTEGER NOT NULL,
    measured_at         TEXT NOT NULL,
    PRIMARY KEY (judge_model, check_name, planted_class, rubric_version)
);";

/// One `judge_trust` row: one judge model, check and applicable class.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustRow {
    /// `""` for a deterministic check.
    pub judge_model: String,
    pub check_name: String,
    pub planted_class: String,
    pub rubric_version: String,
    /// Share of applicable traces where the check flags the mutant AND passes
    /// the unmutated original. The trust metric.
    pub paired_catch_rate: f64,
    pub raw_catch_rate: f64,
    /// Flag rate on the unmutated originals: a false-alarm proxy.
    pub flag_rate: f64,
    /// One-sided 95% Wilson lower bound on `paired_catch_rate`.
    pub wilson_lower: f64,
    /// Distinct traces where the mutation was generated and both sides scored.
    pub sample_size: usize,
    /// RFC 3339.
    pub measured_at: String,
}

/// SQLite-backed `judge_trust` table, in `eval-cache.db` beside `eval_scores`.
pub struct TrustStore {
    conn: Connection,
}

impl TrustStore {
    pub fn open(path: &Path) -> Result<Self> {
        log::debug!("TrustStore::open: path={}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating eval cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening judge trust {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("setting judge trust busy timeout")?;
        conn.execute_batch(CREATE_TABLE).context("creating judge_trust table")?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory judge trust")?;
        conn.execute_batch(CREATE_TABLE).context("creating judge_trust table")?;
        Ok(Self { conn })
    }

    /// Persist a row. A later measurement of the same judge, check, class and
    /// rubric replaces the earlier one; rows on other rubric versions stay as
    /// history and are never current.
    pub fn put(&self, row: &TrustRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO judge_trust
                 (judge_model, check_name, planted_class, rubric_version, paired_catch_rate,
                  raw_catch_rate, flag_rate, wilson_lower, sample_size, measured_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    row.judge_model,
                    row.check_name,
                    row.planted_class,
                    row.rubric_version,
                    row.paired_catch_rate,
                    row.raw_catch_rate,
                    row.flag_rate,
                    row.wilson_lower,
                    row.sample_size as i64,
                    row.measured_at,
                ],
            )
            .with_context(|| {
                format!(
                    "inserting judge trust {} {} {}",
                    row.judge_model, row.check_name, row.planted_class
                )
            })?;
        Ok(())
    }

    /// Every row, ordered by key.
    pub fn all(&self) -> Result<Vec<TrustRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT judge_model, check_name, planted_class, rubric_version, paired_catch_rate,
                        raw_catch_rate, flag_rate, wilson_lower, sample_size, measured_at
                 FROM judge_trust
                 ORDER BY judge_model, check_name, planted_class, rubric_version",
            )
            .context("preparing judge trust read")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(TrustRow {
                    judge_model: r.get(0)?,
                    check_name: r.get(1)?,
                    planted_class: r.get(2)?,
                    rubric_version: r.get(3)?,
                    paired_catch_rate: r.get(4)?,
                    raw_catch_rate: r.get(5)?,
                    flag_rate: r.get(6)?,
                    wilson_lower: r.get(7)?,
                    sample_size: r.get::<_, i64>(8)? as usize,
                    measured_at: r.get(9)?,
                })
            })
            .context("reading judge trust")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("reading a judge trust row")
    }
}

/// Why one applicable class keeps a judge check untrusted.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustFailure {
    pub class: PlantedClass,
    pub reason: String,
}

/// The trust state of one check for one judge model.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustState {
    /// A deterministic check: no model call, never gated.
    Ungated,
    Trusted,
    /// Each failing class and why. Never empty.
    Untrusted(Vec<TrustFailure>),
}

impl TrustState {
    /// True when the check's failures may act: trusted, or ungated.
    pub fn may_act(&self) -> bool {
        matches!(self, Self::Trusted | Self::Ungated)
    }
}

/// Apply the trust rule to `check` for `judge_model` over `rows` (any set of
/// `judge_trust` rows; the matching ones are picked out here).
///
/// `Ungated` is decided by the CHECK, never by the model string: only a
/// deterministic check makes no model call. An LLM check with an empty model
/// ran under fabric's own default model (no `-m`), which no `--inject`
/// measured, so it is untrusted on every class it can see.
pub fn judge_check_trust(judge_model: &str, check: &str, rows: &[TrustRow], policy: &TrustConfig) -> TrustState {
    if DETERMINISTIC_CHECKS.contains(&check) {
        return TrustState::Ungated;
    }
    let classes = applicable_classes(check);
    if judge_model.trim().is_empty() {
        return TrustState::Untrusted(
            classes
                .into_iter()
                .map(|class| TrustFailure {
                    class,
                    reason: "no judge model named: fabric's default model is never trusted".to_string(),
                })
                .collect(),
        );
    }
    let Some(current) = rubric_version_of(check) else {
        return TrustState::Untrusted(
            classes
                .into_iter()
                .map(|class| TrustFailure {
                    class,
                    reason: format!("unknown check '{check}'"),
                })
                .collect(),
        );
    };
    let mut failures = Vec::new();
    for class in classes {
        let of_class =
            |r: &&TrustRow| r.judge_model == judge_model && r.check_name == check && r.planted_class == class.as_str();
        let Some(row) = rows.iter().filter(of_class).find(|r| r.rubric_version == current) else {
            let older: Vec<&str> = rows
                .iter()
                .filter(of_class)
                .map(|r| r.rubric_version.as_str())
                .collect();
            let reason = if older.is_empty() {
                "never measured".to_string()
            } else {
                format!("measured only on rubric {}, current is {current}", older.join(", "))
            };
            failures.push(TrustFailure { class, reason });
            continue;
        };
        if row.sample_size < policy.min_samples {
            failures.push(TrustFailure {
                class,
                reason: format!("n {} < min-samples {}", row.sample_size, policy.min_samples),
            });
        }
        if row.paired_catch_rate < policy.min_paired_catch_rate {
            failures.push(TrustFailure {
                class,
                reason: format!(
                    "paired catch {:.2} < {:.2}",
                    row.paired_catch_rate, policy.min_paired_catch_rate
                ),
            });
        }
    }
    if failures.is_empty() {
        TrustState::Trusted
    } else {
        TrustState::Untrusted(failures)
    }
}

#[cfg(test)]
mod tests;
