//! Resumable bulk scoring: score notes chunk by chunk until the day's
//! allowance is spent, persist every score in a cursor, and pick up from the
//! cursor on the next run (design doc
//! `2026-09-25-described-tag-classification.md`, Phase 2).
//!
//! A whole-vault pass costs about $0.61 and the free day is $0.50, so a bulk
//! job must be able to stop cleanly and resume the next UTC day. `tag-apply`
//! (Phase 8) owns its cursor file and candidate list; this module owns the
//! loop: what a stored score is still worth, when to stop, and what to write
//! to the ledger.

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use super::spend::DaySpend;
use super::{ClassifierDev, TagInput, clip_classifier_text, sha256_hex};

#[cfg(test)]
mod tests;

/// One note to score, named by a caller-chosen id (a vault path).
#[derive(Debug, Clone, Copy)]
pub struct BulkItem<'a> {
    pub id: &'a str,
    pub input: TagInput<'a>,
}

/// Every score a bulk job has paid for, keyed by item id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreCursor {
    /// `ClassifierDev::scoring_config_hash` the scores were made under.
    pub scoring_config_hash: String,
    #[serde(default)]
    pub scores: BTreeMap<String, CursorScore>,
    /// The last error per item that failed. Not a skip list: a failed item
    /// has no score, so the next run tries it again.
    #[serde(default)]
    pub failed: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorScore {
    pub scores: BTreeMap<String, f32>,
    /// Digest of the wire text the scores were made from.
    pub wire_text_hash: String,
}

impl ScoreCursor {
    /// Read a cursor, or start an empty one when none exists yet.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).wrap_err_with(|| format!("failed to parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).wrap_err_with(|| format!("failed to read {}", path.display())),
        }
    }

    /// Write atomically, so a run killed mid-write leaves the last cursor.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("failed to create {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        vault::note::write_atomic(path, text.as_bytes()).wrap_err_with(|| format!("failed to write {}", path.display()))
    }

    /// Discard every score made under a different scoring config. Returns
    /// whether anything was discarded.
    pub fn reconcile(&mut self, scoring_config_hash: &str) -> bool {
        if self.scoring_config_hash == scoring_config_hash {
            return false;
        }
        let discarded = !self.scores.is_empty() || !self.failed.is_empty();
        log::info!(
            "ScoreCursor::reconcile: scoring config changed; discarding {} scores",
            self.scores.len()
        );
        self.scores.clear();
        self.failed.clear();
        self.scoring_config_hash = scoring_config_hash.to_string();
        discarded
    }

    /// Whether `id` has a score made from exactly this wire text.
    pub fn holds(&self, id: &str, wire_text_hash: &str) -> bool {
        self.scores.get(id).is_some_and(|s| s.wire_text_hash == wire_text_hash)
    }
}

/// Digest of the exact string a note sends, so a note edited between runs
/// is scored again rather than served its old score.
pub fn wire_text_hash(input: &TagInput) -> String {
    sha256_hex(clip_classifier_text(input.title, input.text).as_bytes())
}

/// Why a run stopped before scoring everything it was given.
#[derive(Debug, Clone, PartialEq)]
pub enum BulkStop {
    /// The next chunk would have spent past today's allowance.
    DayAllowance { available_usd: f64 },
    /// The next chunk would have spent past the run's own `--max-usd`.
    MaxUsd { max_usd: f64 },
    /// classifier.dev refused a call over its own limit (429, or the day-cap
    /// 402). The run does not retry into it.
    ServiceLimit(String),
}

/// What one run did. `remaining` counts items it never sent.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkOutcome {
    pub scored: usize,
    pub failed: usize,
    pub remaining: usize,
    /// What classifier.dev reported for this run's calls: the ledger's view.
    pub spent_usd: f64,
    pub stop: Option<BulkStop>,
}

impl std::fmt::Display for BulkOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} scored, {} failed, ${:.4} spent",
            self.scored, self.failed, self.spent_usd
        )?;
        match &self.stop {
            None => Ok(()),
            Some(BulkStop::DayAllowance { available_usd }) => write!(
                f,
                "; today's allowance (${available_usd:.4}) is spent: {} remaining, resumes after 00:00 UTC",
                self.remaining
            ),
            Some(BulkStop::MaxUsd { max_usd }) => {
                write!(
                    f,
                    "; --max-usd ${max_usd:.4} reached: {} remaining, rerun to resume",
                    self.remaining
                )
            }
            Some(BulkStop::ServiceLimit(why)) => write!(
                f,
                "; classifier.dev refused over its own limit ({why}): {} remaining, resumes after 00:00 UTC",
                self.remaining
            ),
        }
    }
}

/// One bulk job's spending rules.
pub struct BulkRun<'a> {
    pub classifier: &'a ClassifierDev,
    pub spend: &'a DaySpend,
    /// The run's own ceiling; it spends at most `min(max_usd, allowance)`.
    pub max_usd: Option<f64>,
    /// The ledger's `path` field for this job's rows.
    pub path: &'a str,
    /// The ledger's `tag` field, when the job is about one tag.
    pub tag: Option<&'a str>,
    /// Slugs to keep per note in the cursor; `None` keeps every slug.
    pub keep: Option<&'a [String]>,
}

impl BulkRun<'_> {
    /// Score every item the cursor lacks, one chunk at a time, while the next
    /// chunk fits the spend limit. After each chunk the ledger gets its calls
    /// and `persist` gets the cursor, so a run that stops anywhere, or a
    /// chunk that fails, keeps everything paid for before it.
    ///
    /// Only I/O on the ledger or the cursor is an `Err`. Running out of
    /// allowance, a failed chunk, and classifier.dev's own limit are all
    /// outcomes.
    pub fn run(
        &self,
        items: &[BulkItem],
        cursor: &mut ScoreCursor,
        persist: &mut dyn FnMut(&ScoreCursor) -> Result<()>,
    ) -> Result<BulkOutcome> {
        cursor.reconcile(&self.classifier.scoring_config_hash());
        let pending: Vec<(&BulkItem, String)> = items
            .iter()
            .map(|item| (item, wire_text_hash(&item.input)))
            .filter(|(item, hash)| !cursor.holds(item.id, hash))
            .collect();

        let allowance = self.spend.allowance()?;
        let (limit, limit_stop) = match self.max_usd {
            Some(max) if max < allowance.available_usd() => (max, BulkStop::MaxUsd { max_usd: max }),
            _ => (
                allowance.available_usd(),
                BulkStop::DayAllowance {
                    available_usd: allowance.available_usd(),
                },
            ),
        };
        log::info!(
            "BulkRun::run: path={} items={} pending={} limit=${limit:.4} ({allowance})",
            self.path,
            items.len(),
            pending.len()
        );

        let texts: Vec<String> = pending
            .iter()
            .map(|(item, _)| clip_classifier_text(item.input.title, item.input.text))
            .collect();
        let estimates = self.classifier.wire_usd(&texts);

        let mut outcome = BulkOutcome {
            scored: 0,
            failed: 0,
            remaining: pending.len(),
            spent_usd: 0.0,
            stop: None,
        };
        // Spend counted against the limit: per chunk, the larger of what the
        // service reported and what the model predicted, so a response that
        // under-reports (or carries no `pricing`) cannot run the day dry.
        let (mut counted, mut estimated_so_far) = (0.0f64, 0.0f64);
        let mut start = 0usize;
        for size in self.classifier.plan_run_chunks(&texts) {
            let chunk = &pending[start..start + size];
            let estimate: f64 = estimates[start..start + size].iter().sum();
            // The model's prediction, scaled up (never down) by what the
            // service has charged against it so far this run.
            let ratio = if estimated_so_far > 0.0 { (counted / estimated_so_far).max(1.0) } else { 1.0 };
            let predicted = estimate * ratio;
            if counted + predicted > limit {
                log::info!(
                    "BulkRun::run: next chunk of {size} predicted ${predicted:.4}; ${counted:.4} of ${limit:.4} counted; stopping"
                );
                outcome.stop = Some(limit_stop.clone());
                break;
            }

            let inputs: Vec<TagInput> = chunk.iter().map(|(item, _)| item.input).collect();
            let batch = self.classifier.score_batch(&inputs);
            let reported: f64 = batch.calls.iter().filter(|c| c.ok).map(|c| c.estimated_usd).sum();
            outcome.spent_usd += reported;
            counted += reported.max(estimate);
            estimated_so_far += estimate;
            let recorded = self.spend.record(&batch.calls, self.path, self.tag);

            for ((item, hash), note) in chunk.iter().zip(batch.notes) {
                match note {
                    Ok(scores) => {
                        let kept = scores
                            .into_iter()
                            .filter(|(slug, _)| self.keep.is_none_or(|keep| keep.contains(slug)))
                            .collect();
                        cursor.scores.insert(
                            item.id.to_string(),
                            CursorScore {
                                scores: kept,
                                wire_text_hash: hash.clone(),
                            },
                        );
                        cursor.failed.remove(item.id);
                        outcome.scored += 1;
                        outcome.remaining -= 1;
                    }
                    // Under a service limit the unscored notes were never
                    // sent, or refused as a group: still pending, not failed.
                    Err(_) if batch.service_limit.is_some() => {}
                    Err(e) => {
                        cursor.failed.insert(item.id.to_string(), format!("{e:#}"));
                        outcome.failed += 1;
                        outcome.remaining -= 1;
                    }
                }
            }
            let persisted = persist(cursor);
            recorded.wrap_err("failed to record classifier spend; the cursor was still persisted")?;
            persisted?;

            if let Some(limit) = batch.service_limit {
                outcome.stop = Some(BulkStop::ServiceLimit(limit));
                break;
            }
            start += size;
        }
        log::info!("BulkRun::run: {outcome}");
        Ok(outcome)
    }
}
