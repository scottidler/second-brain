use std::cell::Cell;
use std::collections::HashMap;

use super::*;
use crate::eval::EvalQuery;

/// Scores by text: a text containing `SOURCE` scores `source`, else `note`.
/// Counts calls; `fail` makes every call an error.
struct TextJudge {
    note: u8,
    source: u8,
    fail: bool,
    calls: Cell<usize>,
}

impl TextJudge {
    fn new(note: u8, source: u8) -> Self {
        Self {
            note,
            source,
            fail: false,
            calls: Cell::new(0),
        }
    }
}

impl RelevanceJudge for TextJudge {
    fn judge(&self, _query: &str, _title: &str, text: &str) -> Result<u8> {
        self.calls.set(self.calls.get() + 1);
        if self.fail {
            return Err(eyre!("vendor down"));
        }
        Ok(if text.contains("SOURCE") { self.source } else { self.note })
    }
}

struct MapReader(HashMap<String, SourceText>);

impl SourceReader for MapReader {
    fn read(&self, trace: &str) -> SourceText {
        self.0.get(trace).cloned().unwrap_or(SourceText::Expired)
    }
}

fn queries(n: usize) -> Queries {
    Queries {
        queries: (0..n)
            .map(|i| EvalQuery {
                id: format!("q{i}"),
                query: format!("query {i}"),
                tags: None,
                calibration: None,
            })
            .collect(),
    }
}

fn hit(path: &str, trace: &str) -> Hit {
    Hit {
        path: path.to_string(),
        title: format!("title {path}"),
        note_text: format!("note body of {path}"),
        note_hash: format!("h-{path}"),
        trace: trace.to_string(),
    }
}

fn opts() -> EvalOpts {
    EvalOpts {
        k: 3,
        ..EvalOpts::default()
    }
}

fn reader(found: &[&str], unreadable: &[&str]) -> MapReader {
    let mut map = HashMap::new();
    for t in found {
        map.insert(t.to_string(), SourceText::Found(format!("SOURCE text of {t}")));
    }
    for t in unreadable {
        map.insert(t.to_string(), SourceText::Unreadable("no envelope".into()));
    }
    MapReader(map)
}

#[test]
fn zero_compared_pairs_is_inconclusive_not_a_loss_count_of_zero() {
    let cache = JudgmentCache::open_memory().expect("cache");
    let judge = TextJudge::new(0, 3);
    let hits = vec![vec![hit("a.md", ""), hit("b.md", "tr-gone"), hit("c.md", "tr-bad")]];
    let report = evaluate(&queries(1), &hits, &reader(&[], &["tr-bad"]), &cache, &judge, &opts()).expect("eval");

    assert_eq!(report.verdict(), LossVerdict::Inconclusive);
    assert_eq!(report.compared(), 0);
    assert_eq!(report.eligible(), 3);
    assert_eq!(judge.calls.get(), 0, "no source was readable, so nothing is judged");
    let text = report.render();
    assert!(text.contains("distillation loss: INCONCLUSIVE"), "{text}");
    assert!(!text.contains("distillation loss: 0 of"), "{text}");
}

#[test]
fn every_judge_call_failing_is_inconclusive_too() {
    let cache = JudgmentCache::open_memory().expect("cache");
    let mut judge = TextJudge::new(0, 3);
    judge.fail = true;
    let hits = vec![vec![hit("a.md", "tr-1")]];
    let report = evaluate(&queries(1), &hits, &reader(&["tr-1"], &[]), &cache, &judge, &opts()).expect("eval");
    assert_eq!(report.verdict(), LossVerdict::Inconclusive);
    assert_eq!(report.outcomes.get(&Outcome::JudgeError), Some(&1));
}

#[test]
fn every_hit_gets_one_outcome_and_only_compared_pairs_are_scored() {
    let cache = JudgmentCache::open_memory().expect("cache");
    let judge = TextJudge::new(1, 3);
    let hits = vec![
        vec![hit("a.md", ""), hit("b.md", "tr-1")],
        vec![hit("c.md", "tr-gone"), hit("d.md", "tr-bad"), hit("b.md", "tr-1")],
    ];
    let report = evaluate(
        &queries(2),
        &hits,
        &reader(&["tr-1"], &["tr-bad"]),
        &cache,
        &judge,
        &opts(),
    )
    .expect("eval");

    assert_eq!(report.eligible(), 5);
    assert_eq!(report.compared(), 2);
    assert_eq!(report.outcomes.get(&Outcome::NoTrace), Some(&1));
    assert_eq!(report.outcomes.get(&Outcome::SourceExpired), Some(&1));
    assert_eq!(report.outcomes.get(&Outcome::SourceUnreadable), Some(&1));
    assert_eq!(report.verdict(), LossVerdict::Measured { losses: 2, compared: 2 });
    assert_eq!(report.losses[0].path, "b.md");
    assert_eq!((report.losses[0].source_score, report.losses[0].note_score), (3, 1));
    assert_eq!(judge.calls.get(), 4, "note and source, per compared pair");

    let text = report.render();
    for needle in [
        "pairs: 5 eligible",
        "2 compared",
        "no-trace           1",
        "source-expired     1",
        "source-unreadable  1",
        "distillation loss: 2 of 2 compared pairs",
        "What it cannot see:",
        "only notes oracle already retrieved",
        "staging window",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
}

#[test]
fn a_note_that_answers_or_a_source_that_does_not_is_no_loss() {
    let cache = JudgmentCache::open_memory().expect("cache");
    for (note, source) in [(2, 3), (0, 1), (3, 3)] {
        let judge = TextJudge::new(note, source);
        let report = evaluate(
            &queries(1),
            &[vec![hit("a.md", "tr-1")]],
            &reader(&["tr-1"], &[]),
            &cache,
            &judge,
            &EvalOpts {
                rebuild_cache: true,
                ..opts()
            },
        )
        .expect("eval");
        assert_eq!(
            report.verdict(),
            LossVerdict::Measured { losses: 0, compared: 1 },
            "note {note} source {source}"
        );
    }
}

#[test]
fn source_and_note_judgments_are_cached_under_distinct_keys() {
    let cache = JudgmentCache::open_memory().expect("cache");
    let hits = vec![vec![hit("a.md", "tr-1")]];
    let r = reader(&["tr-1"], &[]);
    let first = TextJudge::new(1, 3);
    evaluate(&queries(1), &hits, &r, &cache, &first, &opts()).expect("eval");
    assert_eq!(first.calls.get(), 2);

    let second = TextJudge::new(3, 0);
    let report = evaluate(&queries(1), &hits, &r, &cache, &second, &opts()).expect("eval");
    assert_eq!(second.calls.get(), 0, "both judgments come from the cache");
    assert_eq!(
        report.losses.len(),
        1,
        "the cached note 1 / source 3 pair, not a collapsed key"
    );
}

#[test]
fn a_long_source_is_capped_at_the_source_budget_and_counted() {
    let (text, truncated) = prepare_source(&"x".repeat(SOURCE_TEXT_MAX_CHARS + 10));
    assert!(truncated);
    assert_eq!(text.chars().count(), SOURCE_TEXT_MAX_CHARS);
    let (text, truncated) = prepare_source("short");
    assert!(!truncated);
    assert_eq!(text, "short");
}
