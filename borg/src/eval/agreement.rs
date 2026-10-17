//! Cross-vendor agreement: pairwise Cohen's kappa between every judge that
//! scored the same notes.
//!
//! A **rater** is one check under one judge model (`judge-claims` on
//! `gemini-2.5-pro`, `judge-distillation` on `claude-sonnet-5`, ...). Raters
//! are paired on the **current baseline rows** of the same trace at the same
//! distill hash (design: "Kappa pairs current rows on (trace, content_hash,
//! rubric_version, check_name) across judge models"), widened from the same
//! check to every pair of raters, so each judge check is compared with the
//! other vendor and with the other check.
//!
//! Two label levels:
//! - **trace:** does the rater flag the note? The flag rule is
//!   [`super::inject::flags`]'s (any axis below 3; any claim not `supported`).
//! - **claim:** is claim `id` supported? Only `judge-claims` carries a
//!   per-claim label.
//!
//! A row under any other check name (the dropped DeepEval and MiniCheck
//! verdicts still sit in older databases) has no flag rule and is not a rater.
//!
//! Kappa is agreement, not correctness: two judges that share an error agree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::Value;

use super::judge::MAX_SCORE;
use super::scores::{NO_MODEL, ScoreRow, rubric_version_of};
use super::{CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, RUN_KIND_BASELINE};

/// One check under one judge model.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rater {
    pub check: String,
    pub judge_model: String,
}

impl fmt::Display for Rater {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.check, self.judge_model)
    }
}

/// The 2x2 table of two raters' binary labels over shared items. "Positive"
/// is the label the level names: flagged at the trace level, supported at the
/// claim level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgreementTable {
    pub both: usize,
    pub a_only: usize,
    pub b_only: usize,
    pub neither: usize,
}

impl AgreementTable {
    pub fn add(&mut self, a: bool, b: bool) {
        match (a, b) {
            (true, true) => self.both += 1,
            (true, false) => self.a_only += 1,
            (false, true) => self.b_only += 1,
            (false, false) => self.neither += 1,
        }
    }

    pub fn n(&self) -> usize {
        self.both + self.a_only + self.b_only + self.neither
    }

    /// Share of items both raters labelled the same; `None` with no items.
    pub fn observed(&self) -> Option<f64> {
        let n = self.n();
        (n > 0).then(|| (self.both + self.neither) as f64 / n as f64)
    }

    /// Cohen's kappa, `(p_o - p_e) / (1 - p_e)`. `None` when there is no
    /// item, or when chance agreement is 1: both raters gave every item the
    /// same single label, so agreement carries no information beyond the
    /// label's prevalence and kappa is 0/0. That case is reported as
    /// undefined, never as perfect agreement.
    pub fn kappa(&self) -> Option<f64> {
        let n = self.n();
        if n == 0 {
            return None;
        }
        let n = n as f64;
        let p_o = (self.both + self.neither) as f64 / n;
        let p_a = (self.both + self.a_only) as f64 / n;
        let p_b = (self.both + self.b_only) as f64 / n;
        let p_e = p_a * p_b + (1.0 - p_a) * (1.0 - p_b);
        if (1.0 - p_e).abs() < 1e-12 {
            return None;
        }
        Some((p_o - p_e) / (1.0 - p_e))
    }

    /// Share of items rater A labelled positive.
    pub fn a_rate(&self) -> Option<f64> {
        let n = self.n();
        (n > 0).then(|| (self.both + self.a_only) as f64 / n as f64)
    }

    /// Share of items rater B labelled positive.
    pub fn b_rate(&self) -> Option<f64> {
        let n = self.n();
        (n > 0).then(|| (self.both + self.b_only) as f64 / n as f64)
    }
}

/// One pair of raters at one level.
#[derive(Debug, Clone, PartialEq)]
pub struct PairAgreement {
    pub a: Rater,
    pub b: Rater,
    pub table: AgreementTable,
}

/// Every pairwise agreement over one set of notes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgreementReport {
    /// Distinct notes (trace at a distill hash) with at least one rater.
    pub notes: usize,
    /// Notes each rater labelled at the trace level.
    pub raters: BTreeMap<Rater, usize>,
    /// Trace level: "flags the note".
    pub trace_pairs: Vec<PairAgreement>,
    /// Claim level: "the claim is supported".
    pub claim_pairs: Vec<PairAgreement>,
}

/// Is `row` a current, error-free baseline judgment a rater can be read from?
/// Judge checks must be on their current rubric. Deterministic rows make no
/// model call and are not raters.
pub fn is_current_rater_row(row: &ScoreRow) -> bool {
    if row.error.is_some() || row.run_kind != RUN_KIND_BASELINE || !row.run_id.is_empty() {
        return false;
    }
    if row.judge_model == NO_MODEL {
        return false;
    }
    rubric_version_of(&row.check_name).is_some_and(|current| row.rubric_version == current)
}

/// Does the rater behind `row` flag the note? `None` when the row's check has
/// no flag rule here or its metrics cannot be read (it is then not a rater).
pub fn trace_flag(row: &ScoreRow) -> Option<bool> {
    let metrics: Value = serde_json::from_str(&row.metrics_json).ok()?;
    match row.check_name.as_str() {
        CHECK_JUDGE_DISTILLATION => {
            let axis = |name: &str| metrics.get(name).and_then(Value::as_u64);
            let axes = [
                axis("claim_coverage")?,
                axis("anchor_validity")?,
                axis("summary_faithfulness")?,
            ];
            Some(axes.iter().any(|a| *a < u64::from(MAX_SCORE)))
        }
        CHECK_JUDGE_CLAIMS => claim_support(row).map(|claims| claims.values().any(|s| !s)),
        _ => None,
    }
}

/// Per-claim support by claim id (1-based, the id the JSONL contract and the
/// claim judge use). `None` for a rater with no per-claim label.
pub fn claim_support(row: &ScoreRow) -> Option<BTreeMap<usize, bool>> {
    let metrics: Value = serde_json::from_str(&row.metrics_json).ok()?;
    match row.check_name.as_str() {
        CHECK_JUDGE_CLAIMS => metrics.get("verdicts")?.as_array().map(|verdicts| {
            verdicts
                .iter()
                .enumerate()
                .map(|(i, v)| (i + 1, v.as_str() == Some("supported")))
                .collect()
        }),
        _ => None,
    }
}

/// Pairwise agreement over `rows`, which may hold anything: only current
/// rater rows ([`is_current_rater_row`]) are read. Notes are keyed on
/// `(trace, content_hash)`, so a rater that judged an older distillation of a
/// trace is never paired with one that judged the current one.
pub fn agreement(rows: &[ScoreRow]) -> AgreementReport {
    type NoteKey = (String, String);
    let mut flags: BTreeMap<Rater, BTreeMap<NoteKey, bool>> = BTreeMap::new();
    let mut claims: BTreeMap<Rater, BTreeMap<(NoteKey, usize), bool>> = BTreeMap::new();
    let mut notes: BTreeSet<NoteKey> = BTreeSet::new();

    for row in rows.iter().filter(|r| is_current_rater_row(r)) {
        let rater = Rater {
            check: row.check_name.clone(),
            judge_model: row.judge_model.clone(),
        };
        let note = (row.trace.clone(), row.content_hash.clone());
        if let Some(flag) = trace_flag(row) {
            notes.insert(note.clone());
            flags.entry(rater.clone()).or_default().insert(note.clone(), flag);
        }
        if let Some(support) = claim_support(row) {
            let per_claim = claims.entry(rater).or_default();
            for (id, supported) in support {
                per_claim.insert((note.clone(), id), supported);
            }
        }
    }

    let raters = flags.iter().map(|(r, labels)| (r.clone(), labels.len())).collect();
    AgreementReport {
        notes: notes.len(),
        raters,
        trace_pairs: pairs(&flags),
        claim_pairs: pairs(&claims),
    }
}

/// Every unordered pair of raters with at least one shared item.
fn pairs<K: Ord>(labels: &BTreeMap<Rater, BTreeMap<K, bool>>) -> Vec<PairAgreement> {
    let raters: Vec<&Rater> = labels.keys().collect();
    let mut out = Vec::new();
    for (i, a) in raters.iter().enumerate() {
        for b in &raters[i + 1..] {
            let (la, lb) = (&labels[*a], &labels[*b]);
            let mut table = AgreementTable::default();
            for (item, va) in la {
                if let Some(vb) = lb.get(item) {
                    table.add(*va, *vb);
                }
            }
            if table.n() > 0 {
                out.push(PairAgreement {
                    a: (*a).clone(),
                    b: (*b).clone(),
                    table,
                });
            }
        }
    }
    out
}

impl AgreementReport {
    /// The agreement section of the `--inject` report.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "pairwise judge agreement on the {} unmutated originals (Cohen's kappa over current baseline rows; \
             kappa undefined (n/a) when both raters gave every item one label)\n",
            self.notes
        ));
        out.push_str("raters (notes labelled):\n");
        for (rater, n) in &self.raters {
            out.push_str(&format!("  {:<44} {n}\n", rater.to_string()));
        }
        out.push_str("\ntrace level (+ = flags the note):\n");
        render_pairs(&mut out, &self.trace_pairs, "flag");
        out.push_str("\nclaim level (+ = claim supported):\n");
        render_pairs(&mut out, &self.claim_pairs, "supp");
        out
    }
}

fn render_pairs(out: &mut String, pairs: &[PairAgreement], positive: &str) {
    if pairs.is_empty() {
        out.push_str("  (no two raters labelled a shared item)\n");
        return;
    }
    out.push_str(&format!(
        "{:<44} {:<44} {:>5} {:>4} {:>4} {:>4} {:>4} {:>6} {:>6} {:>6} {:>6}\n",
        "rater a",
        "rater b",
        "n",
        "++",
        "+-",
        "-+",
        "--",
        format!("a-{positive}"),
        format!("b-{positive}"),
        "agree",
        "kappa"
    ));
    let rate = |r: Option<f64>| r.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}"));
    for p in pairs {
        let t = p.table;
        out.push_str(&format!(
            "{:<44} {:<44} {:>5} {:>4} {:>4} {:>4} {:>4} {:>6} {:>6} {:>6} {:>6}\n",
            p.a.to_string(),
            p.b.to_string(),
            t.n(),
            t.both,
            t.a_only,
            t.b_only,
            t.neither,
            rate(t.a_rate()),
            rate(t.b_rate()),
            rate(t.observed()),
            rate(t.kappa())
        ));
    }
}

#[cfg(test)]
mod tests;
