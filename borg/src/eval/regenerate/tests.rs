use super::*;
use crate::eval::judge::{AxisScores, ClaimVerdict, MockClaimJudge, MockJudge};
use async_trait::async_trait;
use std::sync::Mutex;
use vault::distilled::{Claim, Distilled, Tool};

/// Fake distiller: records `(kind, transcript, note)` per call and returns a
/// note whose claims and tools depend on whether a capture note was passed, so
/// the two arms grade differently.
#[derive(Default)]
struct FakeDispatch {
    calls: Mutex<Vec<(DistillKind, String, Option<String>)>>,
}

#[async_trait]
impl Dispatch for FakeDispatch {
    async fn distill(&self, kind: DistillKind, inputs: DistillInputs<'_>) -> Result<Distilled> {
        self.calls.lock().expect("lock").push((
            kind,
            inputs.transcript.to_string(),
            inputs.capture_note.map(str::to_string),
        ));
        let (claim, tools) = match inputs.capture_note {
            Some(_) => (
                "noted claim",
                vec![Tool {
                    name: "ripgrep".to_string(),
                    url: None,
                    command: Some("cargo install ripgrep".to_string()),
                }],
            ),
            None => ("bare claim", vec![]),
        };
        Ok(Distilled {
            summary: format!("summary of {}", inputs.transcript),
            claims: vec![Claim {
                text: claim.to_string(),
                ..Default::default()
            }],
            tools,
            ..Default::default()
        })
    }
}

fn fixture(kind: &str, slug: &str, source: &str, note: Option<&str>) -> Fixture {
    Fixture {
        id: format!("{kind}/{slug}"),
        kind: kind.to_string(),
        slug: slug.to_string(),
        source: source.to_string(),
        distilled: Distilled::default(),
        capture_note: note.map(str::to_string),
    }
}

fn scores() -> AxisScores {
    AxisScores {
        claim_coverage: 2,
        anchor_validity: 3,
        summary_faithfulness: 3,
    }
}

#[tokio::test]
async fn regenerate_distills_each_noted_fixture_twice_and_grades_both_arms() {
    let fixtures = vec![
        fixture(
            "repo",
            "r",
            "use ripgrep: cargo install ripgrep",
            Some("want a faster grep"),
        ),
        fixture("video", "v", "a talk", Some("the tokio part")),
        fixture("image", "i", "a slide", None),
    ];
    let dispatch = FakeDispatch::default();
    let regenerated = redistill(&fixtures, &dispatch).await.expect("redistill");

    let calls = dispatch.calls.lock().expect("lock").clone();
    assert_eq!(
        calls,
        vec![
            (
                DistillKind::Repo,
                "use ripgrep: cargo install ripgrep".to_string(),
                None
            ),
            (
                DistillKind::Repo,
                "use ripgrep: cargo install ripgrep".to_string(),
                Some("want a faster grep".to_string())
            ),
            (DistillKind::Video, "a talk".to_string(), None),
            (
                DistillKind::Video,
                "a talk".to_string(),
                Some("the tokio part".to_string())
            ),
        ]
    );
    assert_eq!(regenerated.skipped_no_note, vec!["image/i".to_string()]);

    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores());
    let claim_judge = MockClaimJudge::new(ClaimVerdict::Supported).with("noted", ClaimVerdict::Unsupported);
    let report = grade(&regenerated, &cache, &judge, &claim_judge, &EvalOpts::default()).expect("grade");

    // Same fixture set in both arms, judged in both.
    assert_eq!(report.without_note.total_fixtures, 2);
    assert_eq!(report.with_note.total_fixtures, 2);
    assert_eq!(judge.calls(), 4);
    assert_eq!(claim_judge.calls(), 4);
    assert_eq!(report.without_note.claim_support_overall.unsupported, 0);
    assert_eq!(report.with_note.claim_support_overall.unsupported, 2);

    // Only the noted arm carries tools; the repo source backs its command, the
    // video source does not.
    assert!(report.without_note.tools_grounding.is_empty());
    let text = report.render();
    assert!(text.contains("regenerate: 2 fixtures with a capture note"), "{text}");
    assert!(text.contains("tools-grounding: N/A"), "{text}");
    assert!(
        text.contains("tools-grounding: names 1/2 (50.0%), commands 1/2 (50.0%)  (2 fixtures)"),
        "{text}"
    );
}

#[tokio::test]
async fn regenerate_fails_without_any_capture_note() {
    let fixtures = vec![fixture("video", "v", "a talk", None)];
    let err = redistill(&fixtures, &FakeDispatch::default())
        .await
        .expect_err("nothing to compare");
    assert!(format!("{err:#}").contains("--synth-intents"), "{err:#}");
}

#[tokio::test]
async fn regenerate_rejects_an_unroutable_kind() {
    let fixtures = vec![fixture("podcast", "p", "audio", Some("why"))];
    let err = redistill(&fixtures, &FakeDispatch::default())
        .await
        .expect_err("unknown kind");
    assert!(format!("{err:#}").contains("podcast"), "{err:#}");
}

#[test]
fn every_committed_fixture_kind_routes_to_a_distiller() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures");
    for fx in fixtures::load(&dir).expect("load committed fixtures") {
        distill_kind_for_fixture(&fx.kind).unwrap_or_else(|e| panic!("{}: {e}", fx.id));
    }
}

#[test]
fn grade_rejects_emit_calibration() {
    let regenerated = Regenerated {
        without_note: vec![],
        with_note: vec![],
        skipped_no_note: vec![],
    };
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let opts = EvalOpts {
        emit_calibration: Some(std::path::PathBuf::from("/tmp/x.yml")),
        ..EvalOpts::default()
    };
    let err = grade(
        &regenerated,
        &cache,
        &MockJudge::new(scores()),
        &MockClaimJudge::new(ClaimVerdict::Supported),
        &opts,
    )
    .expect_err("emit-calibration");
    assert!(err.to_string().contains("--emit-calibration"));
}
