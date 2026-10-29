//! `sb oracle eval` — relevance-lift measurement harness.
//!
//! Measures whether graph-augmented retrieval (`graph` / `graph-hybrid`) beats
//! the `hybrid` baseline, using a pooled, blind LLM-judge calibrated against
//! hand labels. Design: `docs/design/2026-06-06-oracle-eval-relevance-lift.md`.
//!
//! With `--compare-db` / `--compare-config` the run is a paired A/B: every
//! target runs on both arms and [`paired`] scores each target on its own
//! A∪B pool (design `docs/design/2026-10-29-retrieval-quality-from-rag-scour.md`).
//! Every pooled note must carry a judgment: a judge failure fails the run,
//! it never scores 0.
//!
//! Library-only: this module returns typed data; `sb` renders it. The judge is
//! injected via the [`judge::RelevanceJudge`] trait, and retrieval ([`retrieve`])
//! is split from scoring ([`evaluate`]) so the scoring pipeline is unit-testable
//! without a live index or an LLM.

pub mod answerability;
pub mod cache;
pub mod calc;
pub mod judge;
pub mod metrics;
pub mod mine;
pub mod paired;
pub mod queries;
pub mod report;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use eyre::{Context, Result, bail, eyre};
use vault::search::SearchIndex;

use crate::config::Config;
use crate::server::{OracleMcpServer, RerankRun};
use crate::tools::SearchMode;

#[cfg(test)]
pub use judge::MockJudge;
pub use judge::{FabricJudge, RelevanceJudge};
pub use paired::{PairedReport, PairedRow, Split, Verdict};
pub use queries::{EvalQuery, Queries};
pub use report::{AblationReport, CalibrationPanel, EvalReport, Latency, ModeReport, RerankTally};

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
    /// Arm B's index DB. Set alone or with `compare_config`, it makes the run
    /// a paired A/B; unset, arm B uses arm A's DB.
    pub compare_db: Option<PathBuf>,
    /// Arm B's `oracle.yml`; unset, arm B uses arm A's config.
    pub compare_config: Option<PathBuf>,
    /// Arm B's p95 retrieval budget for a `ship` verdict. `None` reports
    /// latency without gating on it (the budget is phase-specific).
    pub latency_budget_ms: Option<u64>,
}

impl EvalOpts {
    /// True when either arm-B flag is set.
    pub fn is_paired(&self) -> bool {
        self.compare_db.is_some() || self.compare_config.is_some()
    }
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            queries_path: PathBuf::from("config/eval/queries.yml"),
            k: 10,
            judge_model: String::new(),
            rebuild_cache: false,
            emit_calibration: None,
            compare_db: None,
            compare_config: None,
            latency_budget_ms: None,
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

/// One query's retrieval result: each mode's ranked list (by label), the
/// judge text of every pooled note, each mode's wall-clock retrieval time,
/// and what the configured target's rerank stage did. In a paired run the
/// judge texts live on arm A's run and cover both arms' pools.
#[derive(Debug, Clone, Default)]
pub struct QueryRun {
    pub ranked: BTreeMap<String, Vec<String>>,
    pub texts: BTreeMap<String, JudgeText>,
    /// Wall-clock milliseconds per label, retrieval only (no judging).
    pub latency_ms: BTreeMap<String, f64>,
    /// The `configured` target's rerank stage; `None` when not recorded.
    pub rerank: Option<RerankRun>,
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

/// Run the eval end to end: load queries, retrieve on arm A (and arm B when
/// paired), attach one judge text per pooled note, then evaluate with the
/// production [`FabricJudge`].
pub fn run(config: &Config, opts: &EvalOpts) -> Result<EvalOutcome> {
    tracing::debug!(
        queries_path = %opts.queries_path.display(),
        k = opts.k,
        emit = opts.emit_calibration.is_some(),
        paired = opts.is_paired(),
        "eval::run"
    );
    let queries = Queries::load(&opts.queries_path)?;
    let server = open_arm(config.clone(), &config.db_path())?;
    let cache = cache::JudgmentCache::open(&eval_cache_path(config))?;
    let judge = fabric_judge(config, opts);

    let mut runs = retrieve(&server, &queries, opts)?;
    if !opts.is_paired() {
        attach_texts(&[&server], &mut runs, None)?;
        return evaluate(&queries, &runs, None, &cache, &judge, opts);
    }
    let compare = open_compare_arm(config, opts)?;
    let compare_runs = retrieve(&compare, &queries, opts)?;
    attach_texts(&[&server, &compare], &mut runs, Some(&compare_runs))?;
    evaluate(&queries, &runs, Some(&compare_runs), &cache, &judge, opts)
}

/// One arm: a server over `config` and the index at `db_path`.
fn open_arm(config: Config, db_path: &std::path::Path) -> Result<OracleMcpServer> {
    tracing::debug!(db = %db_path.display(), "eval::open_arm");
    let db = SearchIndex::open(db_path).with_context(|| format!("opening search index at {}", db_path.display()))?;
    Ok(OracleMcpServer::new(config, db))
}

/// Arm B: `--compare-config` (else arm A's config) over `--compare-db` (else
/// arm A's DB). Both paths must be existing files: `Config::load` falls back
/// to defaults on a missing file and `SearchIndex::open` creates an empty DB,
/// and either would silently compare against the wrong arm.
fn open_compare_arm(config: &Config, opts: &EvalOpts) -> Result<OracleMcpServer> {
    let config_b = match &opts.compare_config {
        Some(path) => {
            if !path.is_file() {
                bail!("--compare-config {} is not a file", path.display());
            }
            Config::load(Some(path)).with_context(|| format!("loading --compare-config {}", path.display()))?
        }
        None => config.clone(),
    };
    let db_b = match &opts.compare_db {
        Some(path) => {
            if !path.is_file() {
                bail!("--compare-db {} is not a file", path.display());
            }
            path.clone()
        }
        None => config.db_path(),
    };
    open_arm(config_b, &db_b)
}

/// The production relevance judge: `opts`' model and `config`'s
/// `llm.api-keys`.
pub fn fabric_judge(config: &Config, opts: &EvalOpts) -> FabricJudge {
    FabricJudge::new(opts.judge_model.clone(), config.llm.api_keys.clone())
}

/// For each query, run every mode + the fact ablation + the configured
/// pipeline, and collect the ranked lists and the per-target wall-clock time.
/// Judge texts are attached separately ([`attach_texts`]) so a paired run
/// gives each note one text, whichever arm surfaced it. Holds the DB lock only
/// here, never across the (slow) judging in [`evaluate`].
///
/// The first query runs once untimed before the timed pass, so the one-time
/// model loads (query embedder, cross-encoder) are not charged to whichever
/// target happens to run first. The latencies are steady-state.
pub fn retrieve(server: &OracleMcpServer, queries: &Queries, opts: &EvalOpts) -> Result<Vec<QueryRun>> {
    let nonfact: Vec<String> = {
        let handle = server.db_handle();
        let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
        guard.edge_kinds()?.into_iter().filter(|k| k != "fact").collect()
    };

    if let Some(first) = queries.queries.first() {
        let handle = server.db_handle();
        let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
        tracing::debug!(query = %first.id, "eval::retrieve: untimed warmup");
        run_targets(server, &guard, first, &nonfact, opts)?;
    }

    let mut runs = Vec::with_capacity(queries.queries.len());
    for q in &queries.queries {
        let handle = server.db_handle();
        let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
        runs.push(run_targets(server, &guard, q, &nonfact, opts)?);
    }
    Ok(runs)
}

/// Fill `runs[i].texts` with the judge text of every note pooled for query
/// `i` across `runs` and, when paired, `compare`. Each note's text comes from
/// the first server in `servers` whose index holds it (arm A first), so the
/// text is fixed per note, never per arm. A pooled note no index holds gets no
/// text, and [`evaluate`] fails the run on it.
pub fn attach_texts(servers: &[&OracleMcpServer], runs: &mut [QueryRun], compare: Option<&[QueryRun]>) -> Result<()> {
    let handles: Vec<_> = servers.iter().map(|s| s.db_handle()).collect();
    for (i, run) in runs.iter_mut().enumerate() {
        let mut lists: Vec<Vec<String>> = run.ranked.values().cloned().collect();
        if let Some(b) = compare.and_then(|c| c.get(i)) {
            lists.extend(b.ranked.values().cloned());
        }
        for path in metrics::pool(&lists) {
            for handle in &handles {
                let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
                if let Some(note) = guard.get_note(&path).map_err(|e| eyre!("get_note {path}: {e}"))? {
                    run.texts.insert(path.clone(), judge_text(note));
                    break;
                }
            }
        }
    }
    Ok(())
}

/// The judge's view of one note: summary + claims (body when there is no
/// summary), hashed over exactly what the judge sees.
fn judge_text(note: vault::search::NoteRow) -> JudgeText {
    let claims: Vec<String> = vault::search::parse_body_claims(&note.body)
        .into_iter()
        .map(|c| c.text)
        .collect();
    let (text, truncated) = calc::prepare_note_text(&note.summary, &claims, &note.body, JUDGE_TEXT_MAX_CHARS);
    let content_hash = cache::stable_hash(&format!("{}\n{}", note.title, text));
    JudgeText {
        title: note.title,
        text,
        content_hash,
        truncated,
    }
}

/// Run every target once for `q`: the five modes, the fact ablation and the
/// configured pipeline, timing each. Returns the ranked lists, the
/// latencies and the configured target's rerank outcome; no judge text.
fn run_targets(
    server: &OracleMcpServer,
    db: &SearchIndex,
    q: &EvalQuery,
    nonfact: &[String],
    opts: &EvalOpts,
) -> Result<QueryRun> {
    let tags = q.tags.as_deref();
    let mut run = QueryRun::default();

    for m in MODE_ORDER {
        let started = Instant::now();
        let rows = server
            .run_search_mode(
                db,
                *m,
                &q.query,
                tags,
                false,
                None,
                None,
                opts.k,
                EVAL_EXPAND_HOPS,
                None,
                0.0,
            )
            .map_err(|e| eyre!("run_search_mode {:?}: {e}", m))?;
        record(&mut run, mode_label(*m), &rows, started);
    }

    let started = Instant::now();
    let abl = server
        .run_search_mode(
            db,
            SearchMode::GraphHybrid,
            &q.query,
            tags,
            false,
            None,
            None,
            opts.k,
            EVAL_EXPAND_HOPS,
            Some(nonfact),
            0.0,
        )
        .map_err(|e| eyre!("run_search_mode ablation: {e}"))?;
    record(&mut run, ABLATION_LABEL, &abl, started);

    // The live operator-configured pipeline (the shipped default and any
    // rerank/transform the operator has turned on).
    let started = Instant::now();
    let configured = server
        .run_configured_pipeline_traced(db, &q.query, tags, false, None, None, opts.k)
        .map_err(|e| eyre!("run_configured_pipeline: {e}"))?;
    record(&mut run, CONFIGURED_LABEL, &configured.rows, started);
    run.rerank = Some(configured.rerank);
    Ok(run)
}

/// Store one target's ranked paths and its elapsed time since `started`.
fn record(run: &mut QueryRun, label: &str, rows: &[vault::search::NoteRow], started: Instant) {
    run.latency_ms
        .insert(label.to_string(), started.elapsed().as_secs_f64() * 1000.0);
    run.ranked
        .insert(label.to_string(), rows.iter().map(|r| r.path.clone()).collect());
}

/// Judge each pooled note (cache-first, blind), score each mode's ranked
/// list, and aggregate. `runs` (arm A) and `compare` (arm B, when paired) are
/// aligned with `queries.queries` by index; arm A's runs carry the judge text
/// of every note pooled across both arms.
///
/// Every pooled note must end with a judgment. A judge error or a pooled note
/// with no judge text is collected, the rest of the pool is still judged (and
/// cached, so a rerun retries only the failures), and then the run fails.
/// In `--emit-calibration` mode, write the sheet and short-circuit.
pub fn evaluate(
    queries: &Queries,
    runs: &[QueryRun],
    compare: Option<&[QueryRun]>,
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
    let mut missing: Vec<String> = Vec::new();
    let mut per_query: Vec<metrics::Judgments> = Vec::with_capacity(runs.len());

    if let Some(b) = compare
        && b.len() != runs.len()
    {
        bail!("arm B has {} query runs, arm A has {}", b.len(), runs.len());
    }

    for (i, (q, run)) in queries.queries.iter().zip(runs.iter()).enumerate() {
        let query_hash = cache::stable_hash(&q.query);

        if run.ranked.get("graph-hybrid") != run.ranked.get(ABLATION_LABEL) {
            fact_touched += 1;
        }

        let legacy_pool = metrics::pool(&run.ranked.values().cloned().collect::<Vec<_>>());
        let mut lists: Vec<Vec<String>> = run.ranked.values().cloned().collect();
        if let Some(b) = compare {
            lists.extend(b[i].ranked.values().cloned());
        }

        // Judge every pooled note, cache-first.
        let mut judgments: metrics::Judgments = metrics::Judgments::new();
        for path in metrics::pool(&lists) {
            let Some(jt) = run.texts.get(&path) else {
                missing.push(format!(
                    "{} / {path}: no judge text (note in neither arm's index)",
                    q.id
                ));
                continue;
            };
            let key = cache::CacheKey {
                query_id: &q.id,
                query_hash: &query_hash,
                note_path: &path,
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
                        tracing::warn!(query = %q.id, note = %path, "judge failed: {e:#}");
                        missing.push(format!("{} / {path}: {e:#}", q.id));
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
                if let Some(human) = map.get(&path) {
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

        // The legacy single-arm report scores arm A against arm A's
        // all-target pool, exactly as before pairing existed.
        let legacy: metrics::Judgments = judgments
            .iter()
            .filter(|(p, _)| legacy_pool.binary_search(p).is_ok())
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        for (label, list) in &run.ranked {
            let qs = metrics::score_query(list, &legacy, opts.k as usize, judge::HIT_THRESHOLD);
            scores.entry(label.clone()).or_default().push(qs);
        }
        per_query.push(judgments);
    }

    if !missing.is_empty() {
        const SHOWN: usize = 10;
        bail!(
            "{} pooled note(s) have no judgment; the run fails rather than score them 0. \
             Judged pairs are cached, so a rerun retries only these:\n  {}{}",
            missing.len(),
            missing.iter().take(SHOWN).cloned().collect::<Vec<_>>().join("\n  "),
            if missing.len() > SHOWN {
                format!("\n  ... and {} more", missing.len() - SHOWN)
            } else {
                String::new()
            },
        );
    }

    if let Some(path) = &opts.emit_calibration {
        let yaml = serde_yaml::to_string(&sheet).context("serializing calibration sheet")?;
        std::fs::write(path, yaml).with_context(|| format!("writing calibration sheet {}", path.display()))?;
        tracing::debug!(rows = sheet.len(), path = %path.display(), "wrote calibration sheet");
        return Ok(EvalOutcome::CalibrationSheet(path.clone()));
    }

    let paired = match compare {
        Some(b) => Some(paired_report(queries, runs, b, &per_query, &labels, opts)?),
        None => None,
    };

    let modes: Vec<ModeReport> = labels
        .iter()
        .map(|label| ModeReport {
            mode: label.clone(),
            means: metrics::aggregate(&scores[label]),
            latency: Latency::from_samples(runs.iter().filter_map(|r| r.latency_ms.get(label).copied()).collect()),
            rerank: (label == CONFIGURED_LABEL).then(|| RerankTally::from_runs(runs.iter().filter_map(|r| r.rerank))),
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
        paired,
    };
    Ok(EvalOutcome::Report(Box::new(report)))
}

/// Score every target on both arms, per split, on the target's own A∪B pool.
/// A target present on one arm only is a malformed run and fails loudly; a
/// target absent from both (never true of a live run) contributes no pair.
fn paired_report(
    queries: &Queries,
    runs: &[QueryRun],
    compare: &[QueryRun],
    judgments: &[metrics::Judgments],
    labels: &[String],
    opts: &EvalOpts,
) -> Result<PairedReport> {
    let k = opts.k as usize;
    let mut rows = Vec::with_capacity(labels.len() * Split::ALL.len());
    for label in labels {
        for split in Split::ALL {
            let mut scores = Vec::new();
            let mut lat_a = Vec::new();
            let mut lat_b = Vec::new();
            for (i, q) in queries.queries.iter().enumerate() {
                if Split::of(&q.id) != split {
                    continue;
                }
                let (a, b) = (&runs[i], &compare[i]);
                lat_a.extend(a.latency_ms.get(label).copied());
                lat_b.extend(b.latency_ms.get(label).copied());
                match (a.ranked.get(label), b.ranked.get(label)) {
                    (Some(la), Some(lb)) => scores.extend(paired::paired_ndcg(la, lb, &judgments[i], k)),
                    (None, None) => {}
                    _ => bail!("query {}: target {label} ran on one arm only", q.id),
                }
            }
            rows.push(paired::summarize(
                label,
                split,
                &scores,
                lat_a,
                lat_b,
                opts.latency_budget_ms,
            ));
        }
    }
    Ok(PairedReport {
        compare_db: opts.compare_db.as_ref().map(|p| p.display().to_string()),
        compare_config: opts.compare_config.as_ref().map(|p| p.display().to_string()),
        latency_budget_ms: opts.latency_budget_ms,
        rerank_a: RerankTally::from_runs(runs.iter().filter_map(|r| r.rerank)),
        rerank_b: RerankTally::from_runs(compare.iter().filter_map(|r| r.rerank)),
        rows,
    })
}

#[cfg(test)]
mod tests;
