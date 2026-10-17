use super::*;
use crate::eval::judge::{AxisScores, MockClaimJudge, MockJudge};
use crate::eval::scores::ScoreStore;
use std::sync::Mutex;
use vault::distilled::{Claim, EnumeratedItem, Enumeration};

const SOURCE: &str = "the quick brown fox jumps over the lazy dog near the river bank at dawn";
const GROUNDED: &str = "quick brown fox jumps over the lazy dog";
const UNGROUNDED: &str = "marmalade trombone zeppelin";

fn counts(claims: usize, items: usize) -> NoteCounts {
    NoteCounts {
        claims,
        enumeration_items: items,
        quoted: claims,
        grounded: claims,
        judged: claims,
        supported: claims,
        listicle: None,
    }
}

fn sample(notes: &[NoteCounts]) -> SampleMetrics {
    SampleMetrics::of(notes)
}

#[test]
fn a_candidate_with_20_percent_fewer_claims_per_note_scores_zero() {
    let baseline = sample(&[counts(5, 0), counts(5, 0)]);
    let candidate = sample(&[counts(4, 0), counts(4, 0)]);
    let o = objective(&baseline, &candidate, 0.10).unwrap();
    assert_eq!(o.score, 0.0, "{o:?}");
    assert_eq!(o.floor_breaches.len(), 1, "{o:?}");
    assert!(
        o.floor_breaches[0].starts_with("claims per note 5.00 -> 4.00 (-20%)"),
        "{o:?}"
    );
    // Every factor is perfect: only the floor zeroes it.
    assert_eq!(
        (
            o.factors.quote_grounding,
            o.factors.claim_support,
            o.factors.listicle_survival
        ),
        (1.0, 1.0, 1.0)
    );
    // The same notes without the cut score the full product.
    assert_eq!(objective(&baseline, &baseline, 0.10).unwrap().score, 1.0);
}

#[test]
fn a_cut_of_exactly_the_allowed_share_is_not_a_breach() {
    let baseline = sample(&[counts(10, 0)]);
    let candidate = sample(&[counts(9, 0)]);
    let o = objective(&baseline, &candidate, 0.10).unwrap();
    assert!(o.floor_breaches.is_empty(), "{o:?}");
    assert_eq!(o.score, 1.0);
}

#[test]
fn a_candidate_with_fewer_enumeration_items_scores_zero_too() {
    let baseline = sample(&[counts(4, 10)]);
    let candidate = sample(&[counts(4, 7)]);
    let o = objective(&baseline, &candidate, 0.10).unwrap();
    assert_eq!(o.score, 0.0);
    assert!(o.floor_breaches[0].starts_with("enumeration items per note"), "{o:?}");
    // A baseline with no enumeration puts no floor on items.
    let o = objective(&sample(&[counts(4, 0)]), &sample(&[counts(4, 0)]), 0.10).unwrap();
    assert!(o.floor_breaches.is_empty());
}

#[test]
fn the_score_is_the_product_of_grounding_support_and_listicle_survival() {
    let note = NoteCounts {
        claims: 4,
        enumeration_items: 0,
        quoted: 4,
        grounded: 3,
        judged: 4,
        supported: 2,
        listicle: Some(0.5),
    };
    let o = objective(&sample(&[note]), &sample(&[note]), 0.10).unwrap();
    assert!((o.score - 0.75 * 0.5 * 0.5).abs() < 1e-12, "{o:?}");
}

#[test]
fn dropping_every_quote_cannot_win() {
    let baseline = sample(&[counts(4, 0)]);
    let no_quotes = NoteCounts {
        quoted: 0,
        grounded: 0,
        ..counts(4, 0)
    };
    let o = objective(&baseline, &sample(&[no_quotes]), 0.10).unwrap();
    assert_eq!(o.factors.quote_grounding, 0.0);
    assert_eq!(o.score, 0.0);
    // When the baseline quoted nothing either, the factor is not applicable.
    let o = objective(&sample(&[no_quotes]), &sample(&[no_quotes]), 0.10).unwrap();
    assert_eq!(o.factors.quote_grounding, 1.0);
}

#[test]
fn an_empty_sample_has_no_objective() {
    assert!(objective(&SampleMetrics::default(), &sample(&[counts(1, 0)]), 0.1).is_err());
}

fn fixture(kind: &str, trace: &str, source: &str) -> Fixture {
    Fixture {
        id: format!("{kind}/{trace}"),
        kind: kind.to_string(),
        slug: trace.to_string(),
        source: source.to_string(),
        distilled: Distilled::default(),
        capture_note: None,
    }
}

#[test]
fn the_split_is_disjoint_stable_and_only_takes_traces_that_reach_the_pattern() {
    let long = "word ".repeat(60_000);
    let mut fixtures: Vec<Fixture> = (0..6).map(|i| fixture("video", &format!("v{i}"), SOURCE)).collect();
    fixtures.push(fixture("video", "v-long", &long));
    fixtures.push(fixture("article", "a0", SOURCE));
    let s = split(&fixtures, "distill-video", 3).unwrap();
    assert_eq!((s.kind.as_str(), s.eligible), ("video", 6));
    assert_eq!((s.train.len(), s.holdout.len()), (3, 3));
    assert!(s.train.iter().all(|t| !s.holdout.contains(t)), "{s:?}");
    for t in s.train.iter().chain(&s.holdout) {
        assert!(t.starts_with('v') && t != "v-long", "{s:?}");
    }
    fixtures.reverse();
    assert_eq!(split(&fixtures, "distill-video", 3).unwrap(), s, "order-independent");
    let err = split(&fixtures, "distill-video", 4).unwrap_err();
    assert!(format!("{err}").contains("only 6 staged video traces"), "{err}");
}

#[test]
fn only_single_call_distill_patterns_can_be_candidates() {
    assert_eq!(pattern_kind("distill-video").unwrap(), "video");
    assert_eq!(pattern_kind("distill-repo").unwrap(), "repo");
    for p in ["distill-video-chunk", "distill-article-reduce", "judge-claims"] {
        assert!(pattern_kind(p).is_err(), "{p}");
    }
    assert!(reaches_single_call("repo", &"x".repeat(1_000_000)));
    assert!(!reaches_single_call("video", &"x".repeat(1_000_000)));
}

#[test]
fn a_run_id_is_the_pattern_and_its_text_hash() {
    assert_eq!(run_id("distill-video", "a"), run_id("distill-video", "a"));
    assert_ne!(run_id("distill-video", "a"), run_id("distill-video", "b"));
    assert!(run_id("distill-video", "a").starts_with("distill-video:"));
}

#[derive(Clone, Default)]
struct RecordingFabric {
    patterns: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl FabricCaller for RecordingFabric {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        self.patterns.lock().unwrap().push(request.pattern.clone());
        Ok(String::new())
    }
}

#[tokio::test]
async fn the_candidate_fabric_swaps_only_the_named_pattern_and_counts_it() {
    let inner = RecordingFabric::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let fabric = CandidateFabric {
        inner: PatternDirFabric {
            inner: inner.clone(),
            dir: None,
        },
        pattern: "distill-video".to_string(),
        file: PathBuf::from("/tmp/cand/distill-video.md"),
        calls: calls.clone(),
    };
    for pattern in ["distill-video", "distill-video-chunk"] {
        fabric
            .call(FabricRequest {
                pattern: pattern.to_string(),
                input: String::new(),
                model: String::new(),
                max_chars: 0,
                timeout_secs: 0,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        *inner.patterns.lock().unwrap(),
        vec!["/tmp/cand/distill-video.md", "distill-video-chunk"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Distills by pattern text: a pattern containing `GROUNDED-QUOTES` quotes the
/// source verbatim, anything else quotes words the source never says.
/// `claims` sets the claim count of a pattern containing `CLAIMS=<n>`.
#[derive(Default)]
pub(super) struct FakeDistiller {
    pub calls: AtomicUsize,
    pub fail_trace: Option<String>,
}

#[async_trait]
impl PatternDistiller for FakeDistiller {
    async fn distill(&self, fixture: &Fixture, pattern_file: &Path) -> Result<Distilled> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_trace.as_deref() == Some(fixture.slug.as_str()) {
            bail!("fabric exited 1");
        }
        let text = std::fs::read_to_string(pattern_file)?;
        let quote = if text.contains("GROUNDED-QUOTES") { GROUNDED } else { UNGROUNDED };
        let per_trace = format!("CLAIMS-{}=", fixture.slug);
        let key = if text.contains(&per_trace) { per_trace.as_str() } else { "CLAIMS=" };
        let n = text
            .split_once(key)
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(3);
        Ok(Distilled {
            summary: "a summary".to_string(),
            claims: (0..n)
                .map(|i| Claim {
                    text: format!("claim {i}"),
                    quote: Some(quote.to_string()),
                    ..Default::default()
                })
                .collect(),
            enumeration: Some(Enumeration {
                items: vec![EnumeratedItem::default(); 2],
                ..Default::default()
            }),
            ..Default::default()
        })
    }
}

pub(super) struct Env {
    pub dir: tempfile::TempDir,
    pub judgments: JudgmentCache,
    pub distills: DistillCache,
    pub scores: ScoreStore,
    pub judge: MockJudge,
    pub claim_judge: MockClaimJudge,
    pub opts: EvalOpts,
}

pub(super) fn env() -> Env {
    Env {
        dir: tempfile::tempdir().unwrap(),
        judgments: JudgmentCache::open_memory().unwrap(),
        distills: DistillCache::open_memory().unwrap(),
        scores: ScoreStore::open_memory().unwrap(),
        judge: MockJudge::new(AxisScores {
            claim_coverage: 3,
            anchor_validity: 3,
            summary_faithfulness: 3,
        }),
        claim_judge: MockClaimJudge::new(ClaimVerdict::Supported),
        opts: EvalOpts {
            judge_model: "gemini-2.5-pro".to_string(),
            ..EvalOpts::default()
        },
    }
}

impl Env {
    pub fn grading(&self) -> Grading<'_> {
        Grading {
            judge: &self.judge,
            claim_judge: &self.claim_judge,
            judgments: &self.judgments,
            distills: &self.distills,
            scores: &self.scores,
            opts: &self.opts,
        }
    }

    pub fn arm(&self, name: &str, text: &str) -> Arm {
        let path = self.dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        Arm::load("distill-article", &path).unwrap()
    }
}

fn request<'a>(baseline: &'a Arm, candidate: &'a Arm, fixtures: &'a [Fixture], fresh: bool) -> ScoreRequest<'a> {
    ScoreRequest {
        pattern: "distill-article",
        baseline,
        candidate,
        fixtures,
        distill_model: "claude-sonnet-5",
        max_coverage_cut: 0.10,
        distill_workers: 2,
        fresh,
        scored_at: "2026-10-17T00:00:00Z",
    }
}

#[tokio::test]
async fn scoring_writes_candidate_rows_per_arm_and_pays_each_distill_once() {
    let env = env();
    let baseline = env.arm("base.md", "the current pattern");
    let candidate = env.arm("cand.md", "a proposal GROUNDED-QUOTES");
    let fixtures = vec![fixture("article", "a1", SOURCE), fixture("article", "a2", SOURCE)];
    let distiller = FakeDistiller::default();

    let s = score(
        &request(&baseline, &candidate, &fixtures, false),
        &distiller,
        &env.grading(),
    )
    .await
    .unwrap();
    assert_eq!(distiller.calls.load(Ordering::SeqCst), 4, "2 traces x 2 arms");
    assert!(s.complete);
    let (b, c) = (s.baseline.clone().unwrap(), s.candidate.clone().unwrap());
    assert_eq!(b.factors.quote_grounding, 0.0);
    assert_eq!(c.factors.quote_grounding, 1.0);
    assert!(s.candidate_wins(), "{}", s.render());
    assert!(
        s.traces[0].feedback.is_empty(),
        "a clean candidate note has no feedback: {:?}",
        s.traces[0].feedback
    );

    let rows = env.scores.rows_for("a1").unwrap();
    let run_ids: std::collections::BTreeSet<(&str, &str)> =
        rows.iter().map(|r| (r.run_kind.as_str(), r.run_id.as_str())).collect();
    assert_eq!(
        run_ids,
        [
            (RUN_KIND_CANDIDATE, baseline.run_id.as_str()),
            (RUN_KIND_CANDIDATE, candidate.run_id.as_str()),
        ]
        .into_iter()
        .collect()
    );
    assert!(rows.iter().all(|r| r.run_kind != crate::eval::RUN_KIND_BASELINE));

    // The same arms again: every distill is a cache hit.
    let again = score(
        &request(&baseline, &candidate, &fixtures, false),
        &distiller,
        &env.grading(),
    )
    .await
    .unwrap();
    assert_eq!(distiller.calls.load(Ordering::SeqCst), 4);
    assert_eq!(again.candidate, s.candidate);
    // A fresh run re-distills both arms.
    score(
        &request(&baseline, &candidate, &fixtures, true),
        &distiller,
        &env.grading(),
    )
    .await
    .unwrap();
    assert_eq!(distiller.calls.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn a_candidate_that_cuts_claims_loses_on_the_floor_and_says_so() {
    let env = env();
    let baseline = env.arm("base.md", "the current pattern CLAIMS=5");
    let candidate = env.arm("cand.md", "GROUNDED-QUOTES CLAIMS=4");
    let fixtures = vec![fixture("article", "a1", SOURCE)];
    let s = score(
        &request(&baseline, &candidate, &fixtures, false),
        &FakeDistiller::default(),
        &env.grading(),
    )
    .await
    .unwrap();
    let c = s.candidate.clone().unwrap();
    assert_eq!(c.score, 0.0);
    assert!(!s.candidate_wins());
    assert_eq!(s.traces[0].score, Some(0.0));
    assert!(
        s.traces[0]
            .feedback
            .iter()
            .any(|f| f.starts_with("coverage floor (sample): claims per note")),
        "{:?}",
        s.traces[0].feedback
    );
}

#[tokio::test]
async fn a_distill_error_leaves_the_sample_incomplete_with_no_objective() {
    let env = env();
    let baseline = env.arm("base.md", "the current pattern");
    let candidate = env.arm("cand.md", "GROUNDED-QUOTES");
    let fixtures = vec![fixture("article", "a1", SOURCE), fixture("article", "a2", SOURCE)];
    let distiller = FakeDistiller {
        fail_trace: Some("a2".to_string()),
        ..Default::default()
    };
    let s = score(
        &request(&baseline, &candidate, &fixtures, false),
        &distiller,
        &env.grading(),
    )
    .await
    .unwrap();
    assert!(!s.complete);
    assert_eq!((s.baseline.clone(), s.candidate.clone()), (None, None));
    let a2 = s.traces.iter().find(|t| t.trace == "a2").unwrap();
    assert_eq!(a2.score, None);
    assert!(
        a2.errors.iter().any(|e| e.contains("fabric exited 1")),
        "{:?}",
        a2.errors
    );
    assert!(s.render().contains("INCOMPLETE"));
}

#[tokio::test]
async fn scoring_refuses_a_no_judge_run() {
    let mut env = env();
    env.opts.no_judge = true;
    let baseline = env.arm("base.md", "a");
    let candidate = env.arm("cand.md", "b");
    let fixtures = vec![fixture("article", "a1", SOURCE)];
    assert!(
        score(
            &request(&baseline, &candidate, &fixtures, false),
            &FakeDistiller::default(),
            &env.grading()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn one_note_losing_claims_is_feedback_when_the_sample_average_holds_the_floor() {
    let env = env();
    let baseline = env.arm("base.md", "the current pattern CLAIMS=10");
    // a2 alone drops 10 -> 8 claims (-20%); the sample drops 30 -> 28 (-6.7%).
    let candidate = env.arm("cand.md", "GROUNDED-QUOTES CLAIMS=10 CLAIMS-a2=8");
    let fixtures = vec![
        fixture("article", "a1", SOURCE),
        fixture("article", "a2", SOURCE),
        fixture("article", "a3", SOURCE),
    ];
    let s = score(
        &request(&baseline, &candidate, &fixtures, false),
        &FakeDistiller::default(),
        &env.grading(),
    )
    .await
    .unwrap();
    // 30 -> 28 claims over the sample (-6.7%): no breach.
    assert!(s.candidate.clone().unwrap().floor_breaches.is_empty());
    let a2 = s.traces.iter().find(|t| t.trace == "a2").unwrap();
    assert_eq!(a2.score, Some(1.0), "a note-level cut does not zero the note");
    assert!(
        a2.feedback
            .iter()
            .any(|f| f.starts_with("this note: claims per note 10.00 -> 8.00")),
        "{:?}",
        a2.feedback
    );
    assert!(s.candidate_wins(), "{}", s.render());
}

#[tokio::test]
async fn a_candidate_identical_to_the_baseline_is_distilled_once() {
    let env = env();
    let baseline = env.arm("base.md", "the current pattern");
    let same = env.arm("same.md", "the current pattern");
    let fixtures = vec![fixture("article", "a1", SOURCE), fixture("article", "a2", SOURCE)];
    let distiller = FakeDistiller::default();
    let s = score(&request(&baseline, &same, &fixtures, false), &distiller, &env.grading())
        .await
        .unwrap();
    assert_eq!(
        distiller.calls.load(Ordering::SeqCst),
        2,
        "one distill per trace, not per arm"
    );
    assert_eq!(s.baseline, s.candidate);
    let hashes: std::collections::BTreeSet<String> = env
        .scores
        .rows_for("a1")
        .unwrap()
        .into_iter()
        .map(|r| r.content_hash)
        .collect();
    assert_eq!(hashes.len(), 1, "one note, one distill hash");
}
