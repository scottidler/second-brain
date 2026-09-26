//! classifier.dev spend: the ledger bulk paths write, and the day allowance
//! they spend against (design doc `2026-09-25-described-tag-classification.md`,
//! Phase 2).
//!
//! The keyless tier caps free spend at $0.50 per IP per UTC day, and borg's
//! live ingest shares that day without writing the ledger. So a bulk run (more
//! than one note) may spend `daily-usd-budget` minus what the ledger holds for
//! today minus `ingest-reserve-usd`, and classifier.dev's own day-cap refusal
//! stays the backstop behind it.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{CallUsage, TagsClassifierConfig};

#[cfg(test)]
mod tests;

/// The UTC day, behind a port so tests can run "tomorrow".
pub trait Clock: Send + Sync {
    fn today_utc(&self) -> NaiveDate;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn today_utc(&self) -> NaiveDate {
        chrono::Utc::now().date_naive()
    }
}

/// A clock the test sets, and moves to the next UTC day.
#[cfg(test)]
pub(crate) struct FakeClock(std::sync::Mutex<NaiveDate>);

#[cfg(test)]
impl FakeClock {
    pub(crate) fn at(day: &str) -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(day.parse().expect("date"))))
    }

    pub(crate) fn advance_one_day(&self) {
        let mut day = self.0.lock().expect("lock");
        *day = day.succ_opt().expect("next day");
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn today_utc(&self) -> NaiveDate {
        *self.0.lock().expect("lock")
    }
}

/// One ledger line: one successful bulk classifier call. `usd` is the
/// response's `pricing.estimated_usd`, never our estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendRow {
    pub day: NaiveDate,
    pub usd: f64,
    pub tokens: u64,
    /// The bulk path that spent it: `retag`, `tag-apply`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Append-only JSON-lines file of `SpendRow`s.
#[derive(Debug, Clone)]
pub struct SpendLedger {
    path: PathBuf,
}

impl SpendLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The shared ledger, `vault::paths::classifier_spend_ledger()`.
    pub fn shared() -> Self {
        Self::new(vault::paths::classifier_spend_ledger())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What the ledger records for `day`. A missing file is a day with no
    /// bulk spend; a line that does not parse is an error, because guessing
    /// low would let a run spend into the cap.
    pub fn day_total(&self, day: NaiveDate) -> Result<f64> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("SpendLedger::day_total: no ledger at {}", self.path.display());
                return Ok(0.0);
            }
            Err(e) => return Err(e).wrap_err_with(|| format!("failed to open {}", self.path.display())),
        };
        let mut total = 0.0;
        for (n, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line = line.wrap_err_with(|| format!("failed to read {}", self.path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let row: SpendRow = serde_json::from_str(&line)
                .wrap_err_with(|| format!("{}:{}: not a spend row: {line}", self.path.display(), n + 1))?;
            if row.day == day {
                total += row.usd;
            }
        }
        log::debug!("SpendLedger::day_total: day={day} total={total}");
        Ok(total)
    }

    /// Append rows, creating the file and its directory on first use.
    pub fn append(&self, rows: &[SpendRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("failed to create {}", dir.display()))?;
        }
        let mut lines = String::new();
        for row in rows {
            lines.push_str(&serde_json::to_string(row)?);
            lines.push('\n');
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .wrap_err_with(|| format!("failed to open {} for append", self.path.display()))?;
        file.write_all(lines.as_bytes())
            .wrap_err_with(|| format!("failed to append to {}", self.path.display()))?;
        log::debug!("SpendLedger::append: rows={} path={}", rows.len(), self.path.display());
        Ok(())
    }
}

/// What a bulk run may spend today, and the numbers it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Allowance {
    pub day: NaiveDate,
    pub budget_usd: f64,
    pub spent_usd: f64,
    pub reserve_usd: f64,
}

impl Allowance {
    /// `budget - spent - reserve`, never below zero.
    pub fn available_usd(&self) -> f64 {
        (self.budget_usd - self.spent_usd - self.reserve_usd).max(0.0)
    }
}

impl std::fmt::Display for Allowance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "${:.4} left of the ${:.2} day ({} UTC: ${:.4} spent by bulk runs, ${:.2} reserved for ingest)",
            self.available_usd(),
            self.budget_usd,
            self.day,
            self.spent_usd,
            self.reserve_usd
        )
    }
}

/// The ledger, the clock, and the two budget keys: everything a bulk path
/// needs to know what it may spend and to record what it did.
#[derive(Clone)]
pub struct DaySpend {
    ledger: SpendLedger,
    clock: Arc<dyn Clock>,
    daily_usd_budget: f64,
    ingest_reserve_usd: f64,
}

impl DaySpend {
    pub fn new(ledger: SpendLedger, clock: Arc<dyn Clock>, cfg: &TagsClassifierConfig) -> Self {
        Self {
            ledger,
            clock,
            daily_usd_budget: cfg.daily_usd_budget,
            ingest_reserve_usd: cfg.ingest_reserve_usd,
        }
    }

    pub fn ledger(&self) -> &SpendLedger {
        &self.ledger
    }

    pub fn allowance(&self) -> Result<Allowance> {
        let day = self.clock.today_utc();
        Ok(Allowance {
            day,
            budget_usd: self.daily_usd_budget,
            spent_usd: self.ledger.day_total(day)?,
            reserve_usd: self.ingest_reserve_usd,
        })
    }

    /// One ledger row per call that succeeded. A refused or failed call
    /// carried no `pricing`, so it has nothing to record.
    pub fn record(&self, calls: &[CallUsage], path: &str, tag: Option<&str>) -> Result<()> {
        let day = self.clock.today_utc();
        let rows: Vec<SpendRow> = calls
            .iter()
            .filter(|c| c.ok)
            .map(|c| SpendRow {
                day,
                usd: c.estimated_usd,
                tokens: c.input_tokens,
                path: path.to_string(),
                tag: tag.map(str::to_string),
            })
            .collect();
        self.ledger.append(&rows)
    }
}
