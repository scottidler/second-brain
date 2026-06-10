//! `sb oracle eval` — relevance-lift measurement harness.
//!
//! Measures whether graph-augmented retrieval (`graph` / `graph-hybrid`) beats
//! the `hybrid` baseline, using a pooled, blind LLM-judge calibrated against
//! hand labels. Design: `docs/design/2026-06-06-oracle-eval-relevance-lift.md`.
//!
//! Library-only: this module returns typed data; `sb` renders it. The judge is
//! injected via the [`judge::RelevanceJudge`] trait, and retrieval ([`retrieve`])
//! is split from scoring ([`evaluate`]) so the scoring pipeline is unit-testable
//! without a live index or an LLM.

pub mod cache;
pub mod calc;
pub mod judge;
pub mod metrics;
pub mod queries;
pub mod report;

use std::collections::BTreeMap;
use std::path::PathBuf;

use eyre::{Context, Result, eyre};
use vault::search::SearchIndex;

use crate::config::Config;
use crate::server::OracleMcpServer;
use crate::tools::SearchMode;

pub use judge::{FabricJudge, MockJudge, RelevanceJudge};
pub use queries::{EvalQuery, Queries};
pub use report::{AblationReport, CalibrationPanel, EvalReport, ModeReport};

/// Fixed hop budget for graph modes during eval: 2 so 2-hop fact paths
/// (`seed -> hub -> fact -> hub`) are exercised — the ablation needs this too.
const EVAL_EXPAND_HOPS: u8 = 2;
/// Char budget for the note text shown to the judge.
const JUDGE_TEXT_MAX_CHARS: usize = 8_000;
/// Label for the fact-layer ablation variant.
const ABLATION_LABEL: &str = "graph-hybrid (no fact)";
/// Label for the live operator-configured pipeline (`run_configured_pipeline`).
/// Lets the operator measure the shipped pipeline, not only the 5 legacy modes.
const CONFIGURED_LABEL: &str = "configured";
/// Mode rows in report order (the five standard modes; ablation appended last).
const MODE_ORDER: &[SearchMode] = &[
    SearchMode::Bm25,
    SearchMode::Vector,
    SearchMode::Hybrid,
    SearchMode::Graph,
    SearchMode::GraphHybrid,
];

/// CLI-derived options for an eval run.
#[derive(Debug, Clone)]
pub struct EvalOpts {
    pub queries_path: PathBuf,
    pub k: u32,
    pub judge_model: String,
    pub rebuild_cache: bool,
    /// When set, write a fillable calibration sheet to this path and skip metrics.
    pub emit_calibration: Option<PathBuf>,
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            queries_path: PathBuf::from("config/eval/queries.yml"),
            k: 10,
            judge_model: String::new(),
            rebuild_cache: false,
            emit_calibration: None,
        }
    }
}

/// The text of one pooled note, prepared for the judge (already wikilink-flattened
/// and truncated). `content_hash` covers exactly what the judge sees.
#[derive(Debug, Clone)]
pub struct JudgeText {
    pub title: String,
    pub text: String,
    pub content_hash: String,
    pub truncated: bool,
}

/// One query's retrieval result: each mode's ranked list (by label) plus the
/// judge text of every pooled note.
#[derive(Debug, Clone, Default)]
pub struct QueryRun {
    pub ranked: BTreeMap<String, Vec<String>>,
    pub texts: BTreeMap<String, JudgeText>,
}

/// Outcome of `run`: either the metrics report, or the path of the calibration
/// sheet that was written (`--emit-calibration` mode).
pub enum EvalOutcome {
    Report(Box<EvalReport>),
    CalibrationSheet(PathBuf),
}

/// The canonical `SearchMode` → wire label. Single source of truth shared by
/// the eval report and the `knowledge_search` MCP response (a `None` mode there
/// maps to `"configured"` at the call site).
pub(crate) fn mode_label(m: SearchMode) -> &'static str {
    match m {
        SearchMode::Bm25 => "bm25",
        SearchMode::Vector => "vector",
        SearchMode::Hybrid => "hybrid",
        SearchMode::Graph => "graph",
        SearchMode::GraphHybrid => "graph-hybrid",
    }
}

/// Mode/ablation/configured labels in report order.
fn mode_labels() -> Vec<String> {
    MODE_ORDER
        .iter()
        .map(|m| mode_label(*m).to_string())
        .chain(std::iter::once(ABLATION_LABEL.to_string()))
        .chain(std::iter::once(CONFIGURED_LABEL.to_string()))
        .collect()
}

/// Judgment cache colocated with the oracle index DB (per-host).
fn eval_cache_path(config: &Config) -> PathBuf {
    // Beside the configured DB. The fallback when the DB path has no parent is
    // the data-dir path, NEVER a relative `eval-cache.db` (the banned class
    // that writes under CWD).
    let db = config.db_path();
    match db.parent() {
        Some(parent) => parent.join("eval-cache.db"),
        None => vault::paths::oracle_eval_cache_path(),
    }
}

/// One row of the `--emit-calibration` sheet the user fills with `human` scores.
#[derive(Debug, serde::Serialize)]
struct CalibrationRow {
    query_id: String,
    query: String,
    note: String,
    judge: u8,
    human: Option<u8>,
}

/// Run the eval end to end: load queries, retrieve, then evaluate with the
/// production [`FabricJudge`].
pub fn run(config: &Config, opts: &EvalOpts) -> Result<EvalOutcome> {
    tracing::debug!(
        queries_path = %opts.queries_path.display(),
        k = opts.k,
        emit = opts.emit_calibration.is_some(),
        "eval::run"
    );
    let queries = Queries::load(&opts.queries_path)?;
    let db = SearchIndex::open(&config.db_path())
        .with_context(|| format!("opening search index at {}", config.db_path().display()))?;
    let server = OracleMcpServer::new(config.clone(), db);
    let cache = cache::JudgmentCache::open(&eval_cache_path(config))?;
    let judge = FabricJudge::new(opts.judge_model.clone());

    let runs = retrieve(&server, &queries, opts)?;
    evaluate(&queries, &runs, &cache, &judge, opts)
}

/// Phase A: for each query, run every mode + the fact ablation and collect the
/// ranked lists and the judge text of every pooled note. Holds the DB lock only
/// here — never across the (slow) judging in [`evaluate`].
pub fn retrieve(server: &OracleMcpServer, queries: &Queries, opts: &EvalOpts) -> Result<Vec<QueryRun>> {
    let nonfact: Vec<String> = {
        let handle = server.db_handle();
        let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
        guard.edge_kinds()?.into_iter().filter(|k| k != "fact").collect()
    };

    let mut runs = Vec::with_capacity(queries.queries.len());
    for q in &queries.queries {
        let domain = q.domain.as_deref();
        let mut run = QueryRun::default();
        let handle = server.db_handle();
        let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
        for m in MODE_ORDER {
            let rows = server
                .run_search_mode(
                    &guard,
                    *m,
                    &q.query,
                    domain,
                    None,
                    None,
                    opts.k,
                    EVAL_EXPAND_HOPS,
                    None,
                    0.0,
                )
                .map_err(|e| eyre!("run_search_mode {:?}: {e}", m))?;
            run.ranked.insert(
                mode_label(*m).to_string(),
                rows.iter().map(|r| r.path.clone()).collect(),
            );
        }
        let abl = server
            .run_search_mode(
                &guard,
                SearchMode::GraphHybrid,
                &q.query,
                domain,
                None,
                None,
                opts.k,
                EVAL_EXPAND_HOPS,
                Some(&nonfact),
                0.0,
            )
            .map_err(|e| eyre!("run_search_mode ablation: {e}"))?;
        run.ranked
            .insert(ABLATION_LABEL.to_string(), abl.iter().map(|r| r.path.clone()).collect());

        // The live operator-configured pipeline (the shipped default and any
        // rerank/transform the operator has turned on).
        let configured = server
            .run_configured_pipeline(&guard, &q.query, domain, None, None, opts.k)
            .map_err(|e| eyre!("run_configured_pipeline: {e}"))?;
        run.ranked.insert(
            CONFIGURED_LABEL.to_string(),
            configured.iter().map(|r| r.path.clone()).collect(),
        );

        let lists: Vec<Vec<String>> = run.ranked.values().cloned().collect();
        for path in metrics::pool(&lists) {
            if let Some(note) = guard.get_note(&path).map_err(|e| eyre!("get_note {path}: {e}"))? {
                let (text, truncated) = calc::prepare_note_text(&note.summary, &note.body, JUDGE_TEXT_MAX_CHARS);
                let content_hash = cache::stable_hash(&format!("{}\n{}", note.title, text));
                run.texts.insert(
                    path,
                    JudgeText {
                        title: note.title,
                        text,
                        content_hash,
                        truncated,
                    },
                );
            }
        }
        runs.push(run);
    }
    Ok(runs)
}

/// Phases B + C: judge each pooled note (cache-first, blind), score each mode's
/// ranked list, and aggregate. `runs` is aligned with `queries.queries` by index.
/// In `--emit-calibration` mode, write the sheet and short-circuit.
pub fn evaluate(
    queries: &Queries,
    runs: &[QueryRun],
    cache: &cache::JudgmentCache,
    judge: &dyn RelevanceJudge,
    opts: &EvalOpts,
) -> Result<EvalOutcome> {
    let labels = mode_labels();
    let mut scores: BTreeMap<String, Vec<metrics::QueryScores>> =
        labels.iter().map(|l| (l.clone(), Vec::new())).collect();
    let mut fact_touched = 0usize;
    let mut total_judgments = 0usize;
    let mut truncated_judgments = 0usize;
    let mut calib_pairs: Vec<(u8, u8)> = Vec::new();
    let mut sheet: Vec<CalibrationRow> = Vec::new();

    for (q, run) in queries.queries.iter().zip(runs.iter()) {
        let query_hash = cache::stable_hash(&q.query);

        if run.ranked.get("graph-hybrid") != run.ranked.get(ABLATION_LABEL) {
            fact_touched += 1;
        }

        // Phase B: judge every pooled note, cache-first.
        let mut judgments: metrics::Judgments = metrics::Judgments::new();
        for (path, jt) in &run.texts {
            let key = cache::CacheKey {
                query_id: &q.id,
                query_hash: &query_hash,
                note_path: path,
                content_hash: &jt.content_hash,
                judge_model: &opts.judge_model,
            };
            let cached = if opts.rebuild_cache { None } else { cache.get(&key)? };
            let cj = match cached {
                Some(cj) => cj,
                None => match judge.judge(&q.query, &jt.title, &jt.text) {
                    Ok(score) => {
                        let cj = cache::CachedJudgment {
                            score,
                            truncated: jt.truncated,
                        };
                        cache.put(&key, cj)?;
                        cj
                    }
                    Err(e) => {
                        tracing::warn!(query = %q.id, note = %path, "judge failed, skipping pair: {e}");
                        continue;
                    }
                },
            };
            total_judgments += 1;
            if cj.truncated {
                truncated_judgments += 1;
            }
            judgments.insert(path.clone(), cj.score);

            if let Some(map) = &q.calibration {
                if let Some(human) = map.get(path) {
                    calib_pairs.push((*human, cj.score));
                }
                if opts.emit_calibration.is_some() {
                    sheet.push(CalibrationRow {
                        query_id: q.id.clone(),
                        query: q.query.clone(),
                        note: path.clone(),
                        judge: cj.score,
                        human: None,
                    });
                }
            }
        }

        // Phase C: score each mode/label against this query's judgments.
        for (label, list) in &run.ranked {
            let qs = metrics::score_query(list, &judgments, opts.k as usize, judge::HIT_THRESHOLD);
            scores.entry(label.clone()).or_default().push(qs);
        }
    }

    if let Some(path) = &opts.emit_calibration {
        let yaml = serde_yaml::to_string(&sheet).context("serializing calibration sheet")?;
        std::fs::write(path, yaml).with_context(|| format!("writing calibration sheet {}", path.display()))?;
        tracing::debug!(rows = sheet.len(), path = %path.display(), "wrote calibration sheet");
        return Ok(EvalOutcome::CalibrationSheet(path.clone()));
    }

    let modes: Vec<ModeReport> = labels
        .iter()
        .map(|label| ModeReport {
            mode: label.clone(),
            means: metrics::aggregate(&scores[label]),
        })
        .collect();
    let gh = metrics::aggregate(&scores["graph-hybrid"]);
    let hy = metrics::aggregate(&scores["hybrid"]);
    let abl = metrics::aggregate(&scores[ABLATION_LABEL]);

    let calibration = (!calib_pairs.is_empty()).then(|| {
        let n = calib_pairs.len() as f64;
        let exact = calib_pairs.iter().filter(|(h, j)| h == j).count() as f64 / n;
        let adjacent = calib_pairs.iter().filter(|(h, j)| h.abs_diff(*j) <= 1).count() as f64 / n;
        let (precision, recall) = calc::boundary_precision_recall(&calib_pairs, judge::HIT_THRESHOLD);
        let kappa = calc::cohens_kappa(&calib_pairs);
        CalibrationPanel {
            pairs: calib_pairs.len(),
            exact_pct: exact,
            adjacent_pct: adjacent,
            boundary_precision: precision,
            boundary_recall: recall,
            kappa,
            trustworthy: (precision + recall) / 2.0 >= report::TRUST_GATE,
        }
    });

    let report = EvalReport {
        k: opts.k,
        judge_model: opts.judge_model.clone(),
        total_queries: queries.queries.len(),
        modes,
        lift_ndcg: gh.ndcg - hy.ndcg,
        lift_recall: gh.recall - hy.recall,
        lift_mrr: gh.mrr - hy.mrr,
        truncated_judgments,
        total_judgments,
        ablation: AblationReport {
            queries_touching_fact: fact_touched,
            total_queries: queries.queries.len(),
            ndcg_lift_vs_ablation: gh.ndcg - abl.ndcg,
            inconclusive: fact_touched == 0,
        },
        calibration,
    };
    Ok(EvalOutcome::Report(Box::new(report)))
}

#[cfg(test)]
mod tests;
