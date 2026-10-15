//! The distillation eval report: per-kind axis means + composite, a judgment
//! summary (total / new / truncated / fallback), and the judge calibration
//! panel — plus a plain-text renderer for `sb` to print.

use crate::config::MAX_NOTE_BYTES;
use crate::eval::calc;
use crate::eval::judge::{AxisScores, ClaimVerdict, HIT_THRESHOLD};

/// Average of judge-vs-human boundary precision/recall at/above which the judge
/// is trusted (identical gate to the oracle eval).
pub const TRUST_GATE: f64 = 0.6;

/// Per-kind (or overall) axis means and composite.
#[derive(Debug, Clone, PartialEq)]
pub struct KindReport {
    pub kind: String,
    pub n: usize,
    pub claim_coverage: f64,
    pub anchor_validity: f64,
    pub summary_faithfulness: f64,
    pub composite: f64,
}

impl KindReport {
    /// Aggregate a set of per-fixture axis scores into means + composite.
    pub fn aggregate(kind: &str, scores: &[AxisScores]) -> Self {
        let n = scores.len();
        if n == 0 {
            return Self {
                kind: kind.to_string(),
                n: 0,
                claim_coverage: 0.0,
                anchor_validity: 0.0,
                summary_faithfulness: 0.0,
                composite: 0.0,
            };
        }
        let denom = n as f64;
        let cc = scores.iter().map(|s| s.claim_coverage as f64).sum::<f64>() / denom;
        let av = scores.iter().map(|s| s.anchor_validity as f64).sum::<f64>() / denom;
        let sf = scores.iter().map(|s| s.summary_faithfulness as f64).sum::<f64>() / denom;
        let composite = scores.iter().map(|s| s.composite()).sum::<f64>() / denom;
        Self {
            kind: kind.to_string(),
            n,
            claim_coverage: cc,
            anchor_validity: av,
            summary_faithfulness: sf,
            composite,
        }
    }
}

/// Judge-vs-human calibration panel (mirrors the oracle eval: report exact +
/// adjacent agreement and the hit-boundary precision/recall, gate on the mean
/// of P/R rather than kappa alone, which the class-imbalanced pool deflates).
#[derive(Debug, Clone)]
pub struct CalibrationPanel {
    pub pairs: usize,
    pub exact_pct: f64,
    pub adjacent_pct: f64,
    pub boundary_precision: f64,
    pub boundary_recall: f64,
    pub kappa: f64,
    pub trustworthy: bool,
}

/// Build the calibration panel from graded `(human, judge)` axis pairs.
/// `None` when there are no hand labels (an uncalibrated run).
pub fn calibration_panel(pairs: &[(u8, u8)]) -> Option<CalibrationPanel> {
    if pairs.is_empty() {
        return None;
    }
    let n = pairs.len() as f64;
    let exact = pairs.iter().filter(|(h, j)| h == j).count() as f64 / n;
    let adjacent = pairs.iter().filter(|(h, j)| h.abs_diff(*j) <= 1).count() as f64 / n;
    let (precision, recall) = calc::boundary_precision_recall(pairs, HIT_THRESHOLD);
    let kappa = calc::cohens_kappa(pairs);
    Some(CalibrationPanel {
        pairs: pairs.len(),
        exact_pct: exact,
        adjacent_pct: adjacent,
        boundary_precision: precision,
        boundary_recall: recall,
        kappa,
        trustworthy: (precision + recall) / 2.0 >= TRUST_GATE,
    })
}

/// Per-fixture listicle-survival result (distillation-output-restore
/// design). Scored ONLY for fixtures whose EXPECTED `distilled.yml` declares
/// a `declared_count` - that declared count IS the authored assertion "this
/// fixture is a listicle and must survive". Fixtures without one are N/A for
/// this axis and never appear here (excluded from the aggregate, not folded
/// in as a 0.0 - see [`calc::listicle_aggregate`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ListicleMetric {
    pub fixture: String,
    pub score: f64,
}

/// Per-fixture note-size gate result: does the fixture's rendered body stay
/// under the publish-path ceiling (`crate::config::MAX_NOTE_BYTES`).
/// Enforced across every fixture, regardless of kind.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteSizeMetric {
    pub fixture: String,
    pub rendered_bytes: usize,
    pub within_ceiling: bool,
}

/// Per-fixture quote grounding: of the claims carrying a verbatim `quote`, how
/// many quotes appear in the source. Only fixtures with at least one quote.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundingMetric {
    pub fixture: String,
    pub kind: String,
    pub grounded: usize,
    pub quoted: usize,
}

/// Per-fixture tools grounding (`calc::tools_grounding`). Only fixtures whose
/// note renders at least one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolsGroundingMetric {
    pub fixture: String,
    pub kind: String,
    pub counts: calc::ToolsGrounding,
}

/// A fixture whose note carries LLM placeholder filler, one reason per field.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactMetric {
    pub fixture: String,
    pub reasons: Vec<String>,
}

/// Per-kind (or overall) tally of per-claim verdicts.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimSupport {
    pub kind: String,
    pub supported: usize,
    pub unsupported: usize,
    pub contradicted: usize,
}

impl ClaimSupport {
    pub fn empty(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            supported: 0,
            unsupported: 0,
            contradicted: 0,
        }
    }

    pub fn add(&mut self, verdict: ClaimVerdict) {
        match verdict {
            ClaimVerdict::Supported => self.supported += 1,
            ClaimVerdict::Unsupported => self.unsupported += 1,
            ClaimVerdict::Contradicted => self.contradicted += 1,
        }
    }

    /// Fold several tallies into one row labeled `kind`.
    pub fn sum(kind: &str, rows: &[ClaimSupport]) -> Self {
        let mut total = Self::empty(kind);
        for r in rows {
            total.supported += r.supported;
            total.unsupported += r.unsupported;
            total.contradicted += r.contradicted;
        }
        total
    }

    pub fn claims(&self) -> usize {
        self.supported + self.unsupported + self.contradicted
    }

    /// Supported fraction; `None` when no claims were judged.
    pub fn support_rate(&self) -> Option<f64> {
        let n = self.claims();
        (n > 0).then(|| self.supported as f64 / n as f64)
    }
}

/// One claim the claim judge did not mark supported.
#[derive(Debug, Clone, PartialEq)]
pub struct FlaggedClaim {
    pub fixture: String,
    pub verdict: ClaimVerdict,
    pub text: String,
}

/// Rows of a per-fixture listing shown before "... N more".
const LIST_LIMIT: usize = 15;
/// Chars of a flagged claim's text shown in the report.
const CLAIM_PREVIEW_CHARS: usize = 140;

/// The full distillation eval result.
#[derive(Debug, Clone)]
pub struct EvalReport {
    pub judge_model: String,
    pub total_fixtures: usize,
    /// Fixtures scored this run (== `total_fixtures` unless a judge call failed).
    pub total_judgments: usize,
    /// Cache misses this run: 0 on a cache-hit-stable re-run.
    pub new_judgments: usize,
    /// Judgments whose source was truncated to the judge budget (low-confidence
    /// coverage — the judge did not see the whole source).
    pub truncated_judgments: usize,
    /// Fixtures whose distilled artifact carried a `fallback_reason`.
    pub fallback_fixtures: usize,
    pub kinds: Vec<KindReport>,
    pub overall: KindReport,
    pub calibration: Option<CalibrationPanel>,
    /// Per-applicable-fixture listicle-survival scores. Empty when
    /// no fixture in the set declares a `declared_count`.
    pub listicle: Vec<ListicleMetric>,
    /// Mean of `listicle` scores. `None` when `listicle` is empty (nothing
    /// applicable this run) - distinct from `Some(0.0)`, a genuine failure.
    pub listicle_aggregate: Option<f64>,
    /// Per-fixture note-size gate result, one row per fixture in the set.
    pub note_size: Vec<NoteSizeMetric>,
    /// False on a `--no-judge` run: the axis and claim tables are absent.
    pub judged: bool,
    /// Claim-judge cache misses this run.
    pub new_claim_judgments: usize,
    /// Quote grounding, one row per fixture that has quotes.
    pub grounding: Vec<GroundingMetric>,
    /// Tools grounding, one row per fixture whose note renders a tool.
    pub tools_grounding: Vec<ToolsGroundingMetric>,
    /// Fixtures carrying placeholder filler.
    pub artifacts: Vec<ArtifactMetric>,
    /// Per-kind claim-verdict tallies (empty on a `--no-judge` run).
    pub claim_support: Vec<ClaimSupport>,
    pub claim_support_overall: ClaimSupport,
    /// Every claim judged unsupported or contradicted.
    pub flagged_claims: Vec<FlaggedClaim>,
}

impl EvalReport {
    /// Plain-text report.
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "distillation eval  ({} fixtures, judge-model={})\n\n",
            self.total_fixtures,
            if self.judge_model.is_empty() { "<fabric default>" } else { &self.judge_model },
        ));

        if self.judged {
            self.render_judged(&mut o);
        } else {
            o.push_str("judges skipped (--no-judge): deterministic metrics only\n");
        }
        self.render_deterministic(&mut o);
        o
    }

    fn render_judged(&self, o: &mut String) {
        // Kind names are fixture directory names (open vocabulary via --fixtures),
        // so size the column to the widest one instead of a fixed 12.
        let width = self
            .kinds
            .iter()
            .map(|k| k.kind.chars().count())
            .chain([self.overall.kind.chars().count(), "kind".len()])
            .max()
            .unwrap_or(0)
            .max(12);
        let rule = "-".repeat(width + 48);
        o.push_str(&format!(
            "{:<width$} {:>4} {:>10} {:>10} {:>10} {:>10}\n",
            "kind", "n", "coverage", "anchor", "summary", "composite"
        ));
        o.push_str(&format!("{rule}\n"));
        for k in &self.kinds {
            o.push_str(&format!(
                "{:<width$} {:>4} {:>10.3} {:>10.3} {:>10.3} {:>10.3}\n",
                k.kind, k.n, k.claim_coverage, k.anchor_validity, k.summary_faithfulness, k.composite,
            ));
        }
        o.push_str(&format!("{rule}\n"));
        o.push_str(&format!(
            "{:<width$} {:>4} {:>10.3} {:>10.3} {:>10.3} {:>10.3}\n",
            self.overall.kind,
            self.overall.n,
            self.overall.claim_coverage,
            self.overall.anchor_validity,
            self.overall.summary_faithfulness,
            self.overall.composite,
        ));

        o.push_str(&format!(
            "\njudgments: {} scored, {} new axis + {} new claim judgments (cache misses), {} from truncated sources, {} fixtures on a distill fallback\n",
            self.total_judgments,
            self.new_judgments,
            self.new_claim_judgments,
            self.truncated_judgments,
            self.fallback_fixtures,
        ));

        o.push_str("\nclaim support (per-claim verdicts from judge-claims):\n");
        o.push_str(&format!(
            "{:<width$} {:>6} {:>10} {:>12} {:>13}\n",
            "kind", "claims", "supported", "unsupported", "contradicted"
        ));
        let rows = self
            .claim_support
            .iter()
            .chain(std::iter::once(&self.claim_support_overall));
        for r in rows {
            let rate = r
                .support_rate()
                .map(|x| format!("{:.1}%", x * 100.0))
                .unwrap_or_else(|| "N/A".to_string());
            o.push_str(&format!(
                "{:<width$} {:>6} {:>10} {:>12} {:>13}\n",
                r.kind,
                r.claims(),
                rate,
                r.unsupported,
                r.contradicted
            ));
        }
        let mut flagged: Vec<&FlaggedClaim> = self.flagged_claims.iter().collect();
        flagged.sort_by_key(|f| (f.verdict != ClaimVerdict::Contradicted, f.fixture.clone()));
        for f in flagged.iter().take(LIST_LIMIT) {
            let label = match f.verdict {
                ClaimVerdict::Contradicted => "CONTRADICTED",
                ClaimVerdict::Unsupported => "unsupported ",
                ClaimVerdict::Supported => "supported   ",
            };
            let preview: String = f.text.chars().take(CLAIM_PREVIEW_CHARS).collect();
            o.push_str(&format!("    {label} {}: {preview}\n", f.fixture));
        }
        if flagged.len() > LIST_LIMIT {
            o.push_str(&format!("    ... {} more\n", flagged.len() - LIST_LIMIT));
        }

        match &self.calibration {
            Some(c) => {
                o.push_str(&format!(
                    "\ncalibration ({} hand-labeled axis pairs): exact {:.0}%  adjacent {:.0}%  \
                     boundary P/R {:.2}/{:.2}  kappa {:.2}  -> {}\n",
                    c.pairs,
                    c.exact_pct * 100.0,
                    c.adjacent_pct * 100.0,
                    c.boundary_precision,
                    c.boundary_recall,
                    c.kappa,
                    if c.trustworthy { "TRUSTWORTHY" } else { "LOW-CONFIDENCE (judge unvalidated)" },
                ));
            }
            None => {
                o.push_str(
                    "\ncalibration: UNCALIBRATED - no hand labels; judge unvalidated. Run --emit-calibration.\n",
                );
            }
        }
    }

    fn render_deterministic(&self, o: &mut String) {
        o.push_str("\ndeterministic metrics (zero judge calls):\n");
        match self.listicle_aggregate {
            Some(agg) => {
                o.push_str(&format!(
                    "  listicle-survival: {:.3}  ({} applicable fixture{})\n",
                    agg,
                    self.listicle.len(),
                    if self.listicle.len() == 1 { "" } else { "s" },
                ));
                // A staged run has hundreds of applicable fixtures: past the
                // list limit, show only the shortfalls.
                let shown: Vec<&ListicleMetric> = if self.listicle.len() <= LIST_LIMIT {
                    self.listicle.iter().collect()
                } else {
                    self.listicle.iter().filter(|m| m.score < 1.0).collect()
                };
                let width = shown.iter().map(|m| m.fixture.chars().count()).max().unwrap_or(0);
                for m in &shown {
                    o.push_str(&format!("    {:<width$} {:.3}\n", m.fixture, m.score));
                }
            }
            None => o.push_str("  listicle-survival: N/A (no fixture declares a declared-count)\n"),
        }
        let failing: Vec<&NoteSizeMetric> = self.note_size.iter().filter(|m| !m.within_ceiling).collect();
        o.push_str(&format!(
            "  note-size: {}/{} within the {}-byte ceiling\n",
            self.note_size.len() - failing.len(),
            self.note_size.len(),
            MAX_NOTE_BYTES,
        ));
        let width = failing.iter().map(|m| m.fixture.chars().count()).max().unwrap_or(0);
        for m in &failing {
            o.push_str(&format!("    FAIL {:<width$} {} bytes\n", m.fixture, m.rendered_bytes));
        }

        let quoted: usize = self.grounding.iter().map(|g| g.quoted).sum();
        let grounded: usize = self.grounding.iter().map(|g| g.grounded).sum();
        if quoted == 0 {
            o.push_str("  quote-grounding: N/A (no claim carries a quote)\n");
        } else {
            o.push_str(&format!(
                "  quote-grounding: {grounded}/{quoted} quotes found in the source ({:.1}%, {} fixtures)\n",
                grounded as f64 / quoted as f64 * 100.0,
                self.grounding.len(),
            ));
            let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
            for g in &self.grounding {
                let e = by_kind.entry(g.kind.as_str()).or_default();
                e.0 += g.grounded;
                e.1 += g.quoted;
            }
            for (kind, (g, q)) in &by_kind {
                o.push_str(&format!(
                    "    {kind:<10} {g}/{q} ({:.1}%)\n",
                    *g as f64 / *q as f64 * 100.0
                ));
            }
            let mut worst: Vec<&GroundingMetric> = self.grounding.iter().filter(|g| g.grounded < g.quoted).collect();
            worst.sort_by(|a, b| {
                let ra = a.grounded as f64 / a.quoted as f64;
                let rb = b.grounded as f64 / b.quoted as f64;
                ra.total_cmp(&rb).then_with(|| a.fixture.cmp(&b.fixture))
            });
            for g in worst.iter().take(LIST_LIMIT) {
                o.push_str(&format!("    LOW {} {}/{}\n", g.fixture, g.grounded, g.quoted));
            }
            if worst.len() > LIST_LIMIT {
                o.push_str(&format!("    ... {} more below 100%\n", worst.len() - LIST_LIMIT));
            }
        }
        self.render_tools_grounding(o);
        o.push_str(&format!(
            "  placeholder-artifacts: {} fixture{}\n",
            self.artifacts.len(),
            if self.artifacts.len() == 1 { "" } else { "s" },
        ));
        for a in self.artifacts.iter().take(LIST_LIMIT) {
            o.push_str(&format!("    {} ({})\n", a.fixture, a.reasons.join("; ")));
        }
    }
}

impl EvalReport {
    fn render_tools_grounding(&self, o: &mut String) {
        if self.tools_grounding.is_empty() {
            o.push_str("  tools-grounding: N/A (no note renders a tool)\n");
            return;
        }
        let total = self
            .tools_grounding
            .iter()
            .fold(calc::ToolsGrounding::default(), |acc, m| add_counts(acc, m.counts));
        o.push_str(&format!(
            "  tools-grounding: {}  ({} fixture{})\n",
            tools_counts_line(total),
            self.tools_grounding.len(),
            if self.tools_grounding.len() == 1 { "" } else { "s" },
        ));
        let mut by_kind: std::collections::BTreeMap<&str, calc::ToolsGrounding> = std::collections::BTreeMap::new();
        for m in &self.tools_grounding {
            let e = by_kind.entry(m.kind.as_str()).or_default();
            *e = add_counts(*e, m.counts);
        }
        for (kind, counts) in &by_kind {
            o.push_str(&format!("    {kind:<10} {}\n", tools_counts_line(*counts)));
        }
        let low: Vec<&ToolsGroundingMetric> = self
            .tools_grounding
            .iter()
            .filter(|m| m.counts.names_grounded < m.counts.names || m.counts.commands_grounded < m.counts.commands)
            .collect();
        for m in low.iter().take(LIST_LIMIT) {
            o.push_str(&format!("    LOW {} {}\n", m.fixture, tools_counts_line(m.counts)));
        }
        if low.len() > LIST_LIMIT {
            o.push_str(&format!("    ... {} more below 100%\n", low.len() - LIST_LIMIT));
        }
    }
}

fn add_counts(a: calc::ToolsGrounding, b: calc::ToolsGrounding) -> calc::ToolsGrounding {
    calc::ToolsGrounding {
        names_grounded: a.names_grounded + b.names_grounded,
        names: a.names + b.names,
        commands_grounded: a.commands_grounded + b.commands_grounded,
        commands: a.commands + b.commands,
    }
}

/// `names g/n (p%), commands g/n (p%)`, `N/A` for a zero denominator.
fn tools_counts_line(c: calc::ToolsGrounding) -> String {
    let pct = |g: usize, n: usize| {
        if n == 0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", g as f64 / n as f64 * 100.0)
        }
    };
    format!(
        "names {}/{} ({}), commands {}/{} ({})",
        c.names_grounded,
        c.names,
        pct(c.names_grounded, c.names),
        c.commands_grounded,
        c.commands,
        pct(c.commands_grounded, c.commands),
    )
}

#[cfg(test)]
mod tests;
