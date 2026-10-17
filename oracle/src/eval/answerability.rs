//! `sb oracle eval --target answerability`: does a note still answer what its
//! source answered?
//!
//! For each query's top hits from the live configured pipeline, the relevance
//! judge grades the note and, separately, the note's verbatim source. A pair
//! where the source scores >= 2 and the note <= 1 is a distillation-loss
//! signal that needs no labels.
//!
//! Sources come through the [`SourceReader`] port: oracle does not depend on
//! borg, so `sb` supplies a reader over borg's staging root. Every hit gets
//! exactly one [`Outcome`]; only `compared` is scored, and zero compared pairs
//! is `inconclusive`, never "no loss".

use std::collections::BTreeMap;

use eyre::{Result, eyre};
use vault::search::SearchIndex;

use super::cache::{self, CacheKey, CachedJudgment, JudgmentCache};
use super::judge::{FabricJudge, HIT_THRESHOLD, RelevanceJudge};
use super::{EvalOpts, Queries, calc};
use crate::config::Config;
use crate::server::OracleMcpServer;

/// Char budget for the note text the judge grades (the lift target's budget).
const NOTE_TEXT_MAX_CHARS: usize = 8_000;
/// Char budget for the source text the judge grades. Sources run long, and a
/// source cut short of its answer would hide a loss, so they get the room
/// fabric's input budget allows.
const SOURCE_TEXT_MAX_CHARS: usize = 22_000;
/// Fabric input budget for the answerability judge: the source budget plus the
/// query and title framing.
const JUDGE_INPUT_MAX_CHARS: usize = 24_000;
/// A note at or below this score, where its source is a hit, is a loss.
const NOTE_LOSS_MAX: u8 = HIT_THRESHOLD - 1;
/// Cache-key prefix that keeps a source judgment from aliasing its note's.
const SOURCE_KEY_PREFIX: &str = "source:";

/// What the source reader found for a trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceText {
    /// The verbatim source the distiller saw.
    Found(String),
    /// The trace has been swept from staging.
    Expired,
    /// Missing artifacts, an unsupported kind, or a parse error; carries why.
    Unreadable(String),
}

/// Reads a note's verbatim source by its borg trace id.
pub trait SourceReader {
    fn read(&self, trace: &str) -> SourceText;
}

/// One retrieved hit, prepared for the judge.
#[derive(Debug, Clone)]
pub struct Hit {
    pub path: String,
    pub title: String,
    /// Flattened, capped note body: the whole note an agent reads.
    pub note_text: String,
    pub note_hash: String,
    /// borg trace id, `""` when the note carries none.
    pub trace: String,
}

/// The one outcome of a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Source and note both judged.
    Compared,
    /// The note's `trace` is empty.
    NoTrace,
    /// The trace has been swept from staging.
    SourceExpired,
    /// The staged trace exists but yields no source.
    SourceUnreadable,
    /// A source was read but a judge call failed; nothing scored.
    JudgeError,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Compared => "compared",
            Outcome::NoTrace => "no-trace",
            Outcome::SourceExpired => "source-expired",
            Outcome::SourceUnreadable => "source-unreadable",
            Outcome::JudgeError => "judge-error",
        }
    }

    const ALL: [Outcome; 5] = [
        Outcome::Compared,
        Outcome::NoTrace,
        Outcome::SourceExpired,
        Outcome::SourceUnreadable,
        Outcome::JudgeError,
    ];
}

/// A compared pair whose source answers the query and whose note does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    pub query_id: String,
    pub query: String,
    pub path: String,
    pub trace: String,
    pub source_score: u8,
    pub note_score: u8,
}

/// The distillation-loss verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LossVerdict {
    /// No pair was compared: nothing is known about loss.
    Inconclusive,
    /// `losses` of `compared` pairs lost the answer.
    Measured { losses: usize, compared: usize },
}

/// The answerability result.
#[derive(Debug, Clone)]
pub struct AnswerabilityReport {
    pub k: u32,
    pub judge_model: String,
    pub total_queries: usize,
    /// Pairs per outcome; their sum is the eligible pair count.
    pub outcomes: BTreeMap<Outcome, usize>,
    /// Compared pairs whose source scored >= `HIT_THRESHOLD`.
    pub source_answers: usize,
    /// Compared pairs whose note scored >= `HIT_THRESHOLD`.
    pub note_answers: usize,
    /// Compared sources cut to the source budget before judging.
    pub sources_truncated: usize,
    pub losses: Vec<Loss>,
}

impl AnswerabilityReport {
    fn count(&self, outcome: Outcome) -> usize {
        self.outcomes.get(&outcome).copied().unwrap_or(0)
    }

    /// Every retrieved `(query, hit)` pair.
    pub fn eligible(&self) -> usize {
        self.outcomes.values().sum()
    }

    pub fn compared(&self) -> usize {
        self.count(Outcome::Compared)
    }

    pub fn verdict(&self) -> LossVerdict {
        match self.compared() {
            0 => LossVerdict::Inconclusive,
            compared => LossVerdict::Measured {
                losses: self.losses.len(),
                compared,
            },
        }
    }

    /// Plain-text report, headed by what it cannot see.
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "answerability eval @ K={}  ({} queries, judge-model={}, retrieval=configured)\n\n",
            self.k,
            self.total_queries,
            if self.judge_model.is_empty() { "<fabric default>" } else { &self.judge_model },
        ));
        o.push_str(
            "What this measures: for each query's top hits, whether the note answers the query\n\
             as well as its own source does (source >= 2 and note <= 1 is a distillation loss).\n\
             What it cannot see:\n  \
             - only notes oracle already retrieved enter the comparison; a distillation that\n    \
             dropped the very terms a query needs never surfaces as a hit, so that loss is\n    \
             not measured here\n  \
             - sources come only from borg's staging window (14 days by default), so the\n    \
             score skews to recent notes\n\n",
        );
        o.push_str(&format!(
            "pairs: {} eligible (every retrieved query-hit pair), {} compared\n",
            self.eligible(),
            self.compared()
        ));
        o.push_str("outcomes:\n");
        for outcome in Outcome::ALL {
            o.push_str(&format!("  {:<18} {}\n", outcome.label(), self.count(outcome)));
        }
        o.push('\n');
        match self.verdict() {
            LossVerdict::Inconclusive => {
                o.push_str("distillation loss: INCONCLUSIVE - 0 compared pairs (no source was judged)\n");
            }
            LossVerdict::Measured { losses, compared } => {
                o.push_str(&format!(
                    "distillation loss: {losses} of {compared} compared pairs ({:.1}%)\n",
                    100.0 * losses as f64 / compared as f64
                ));
                o.push_str(&format!(
                    "  source answers (>= {HIT_THRESHOLD}): {}   note answers (>= {HIT_THRESHOLD}): {}   \
                     sources truncated to {SOURCE_TEXT_MAX_CHARS} chars: {}\n",
                    self.source_answers, self.note_answers, self.sources_truncated
                ));
            }
        }
        if !self.losses.is_empty() {
            o.push_str("\nlosses (source score / note score):\n");
            for l in &self.losses {
                o.push_str(&format!(
                    "  {}/{}  {}  [{}]  {:?}\n",
                    l.source_score, l.note_score, l.path, l.trace, l.query
                ));
            }
        }
        o
    }
}

/// The answerability judge: `judge-relevance` with an input budget that fits a
/// staged source.
pub fn fabric_judge(config: &Config, opts: &EvalOpts) -> FabricJudge {
    let mut judge = super::fabric_judge(config, opts);
    judge.max_chars = JUDGE_INPUT_MAX_CHARS;
    judge
}

/// Run the target end to end over `opts.queries_path`, reading sources
/// through `reader`.
pub fn run(config: &Config, opts: &EvalOpts, reader: &dyn SourceReader) -> Result<AnswerabilityReport> {
    tracing::debug!(queries_path = %opts.queries_path.display(), k = opts.k, "answerability::run");
    let queries = Queries::load(&opts.queries_path)?;
    let db = SearchIndex::open(&config.db_path())
        .map_err(|e| eyre!("opening search index at {}: {e}", config.db_path().display()))?;
    let server = OracleMcpServer::new(config.clone(), db);
    let cache = JudgmentCache::open(&super::eval_cache_path(config))?;
    let judge = fabric_judge(config, opts);
    let hits = retrieve(&server, &queries, opts.k)?;
    evaluate(&queries, &hits, reader, &cache, &judge, opts)
}

/// Each query's top `k` hits from the configured pipeline, aligned with
/// `queries.queries` by index. Holds the DB lock only here.
pub fn retrieve(server: &OracleMcpServer, queries: &Queries, k: u32) -> Result<Vec<Vec<Hit>>> {
    let handle = server.db_handle();
    let guard = handle.lock().map_err(|e| eyre!("db lock poisoned: {e}"))?;
    let mut all = Vec::with_capacity(queries.queries.len());
    for q in &queries.queries {
        let rows = server
            .run_configured_pipeline(&guard, &q.query, q.tags.as_deref(), false, None, None, k)
            .map_err(|e| eyre!("run_configured_pipeline {:?}: {e}", q.query))?;
        let mut hits = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(note) = guard
                .get_note(&row.path)
                .map_err(|e| eyre!("get_note {}: {e}", row.path))?
            else {
                continue;
            };
            let (note_text, _) = calc::prepare_note_text("", &note.body, NOTE_TEXT_MAX_CHARS);
            hits.push(Hit {
                note_hash: cache::stable_hash(&format!("{}\n{}", note.title, note_text)),
                path: note.path,
                title: note.title,
                note_text,
                trace: note.trace,
            });
        }
        all.push(hits);
    }
    Ok(all)
}

/// The judged source text: capped at the source budget. Returns `(text, truncated)`.
fn prepare_source(source: &str) -> (String, bool) {
    let truncated = source.chars().count() > SOURCE_TEXT_MAX_CHARS;
    (source.chars().take(SOURCE_TEXT_MAX_CHARS).collect(), truncated)
}

/// Cache-first judgment. `Err` only for a cache failure; a judge failure is `Ok(None)`.
fn judged(
    cache: &JudgmentCache,
    judge: &dyn RelevanceJudge,
    key: &CacheKey,
    rebuild: bool,
    query: &str,
    title: &str,
    text: &str,
) -> Result<Option<u8>> {
    if !rebuild && let Some(cj) = cache.get(key)? {
        return Ok(Some(cj.score));
    }
    match judge.judge(query, title, text) {
        Ok(score) => {
            cache.put(
                key,
                CachedJudgment {
                    score,
                    truncated: false,
                },
            )?;
            Ok(Some(score))
        }
        Err(e) => {
            tracing::warn!(query = %key.query_id, item = %key.note_path, "answerability judge failed: {e:#}");
            Ok(None)
        }
    }
}

/// Give every hit one outcome, judge note and source for each hit with a
/// readable source, and count the losses. `hits` aligns with `queries.queries`.
pub fn evaluate(
    queries: &Queries,
    hits: &[Vec<Hit>],
    reader: &dyn SourceReader,
    cache: &JudgmentCache,
    judge: &dyn RelevanceJudge,
    opts: &EvalOpts,
) -> Result<AnswerabilityReport> {
    let mut report = AnswerabilityReport {
        k: opts.k,
        judge_model: opts.judge_model.clone(),
        total_queries: queries.queries.len(),
        outcomes: BTreeMap::new(),
        source_answers: 0,
        note_answers: 0,
        sources_truncated: 0,
        losses: Vec::new(),
    };
    for (q, q_hits) in queries.queries.iter().zip(hits) {
        let query_hash = cache::stable_hash(&q.query);
        for hit in q_hits {
            let outcome = if hit.trace.trim().is_empty() {
                Outcome::NoTrace
            } else {
                match reader.read(&hit.trace) {
                    SourceText::Expired => Outcome::SourceExpired,
                    SourceText::Unreadable(why) => {
                        tracing::debug!(trace = %hit.trace, "source unreadable: {why}");
                        Outcome::SourceUnreadable
                    }
                    SourceText::Found(source) => {
                        let (source_text, truncated) = prepare_source(&source);
                        let source_item = format!("{SOURCE_KEY_PREFIX}{}", hit.trace);
                        let source_hash = cache::stable_hash(&format!("{}\n{}", hit.title, source_text));
                        let note_key = CacheKey {
                            query_id: &q.id,
                            query_hash: &query_hash,
                            note_path: &hit.path,
                            content_hash: &hit.note_hash,
                            judge_model: &opts.judge_model,
                        };
                        let source_key = CacheKey {
                            query_id: &q.id,
                            query_hash: &query_hash,
                            note_path: &source_item,
                            content_hash: &source_hash,
                            judge_model: &opts.judge_model,
                        };
                        let note = judged(
                            cache,
                            judge,
                            &note_key,
                            opts.rebuild_cache,
                            &q.query,
                            &hit.title,
                            &hit.note_text,
                        )?;
                        let source = judged(
                            cache,
                            judge,
                            &source_key,
                            opts.rebuild_cache,
                            &q.query,
                            &hit.title,
                            &source_text,
                        )?;
                        match (note, source) {
                            (Some(note_score), Some(source_score)) => {
                                if truncated {
                                    report.sources_truncated += 1;
                                }
                                if source_score >= HIT_THRESHOLD {
                                    report.source_answers += 1;
                                }
                                if note_score >= HIT_THRESHOLD {
                                    report.note_answers += 1;
                                }
                                if source_score >= HIT_THRESHOLD && note_score <= NOTE_LOSS_MAX {
                                    report.losses.push(Loss {
                                        query_id: q.id.clone(),
                                        query: q.query.clone(),
                                        path: hit.path.clone(),
                                        trace: hit.trace.clone(),
                                        source_score,
                                        note_score,
                                    });
                                }
                                Outcome::Compared
                            }
                            _ => Outcome::JudgeError,
                        }
                    }
                }
            };
            *report.outcomes.entry(outcome).or_default() += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
