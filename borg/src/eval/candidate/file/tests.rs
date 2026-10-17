use super::*;
use crate::eval::candidate::tests::{FakeDistiller, env};
use std::sync::atomic::Ordering;

const SOURCE: &str = "the quick brown fox jumps over the lazy dog near the river bank at dawn";
const BASE: &str = "# the current pattern\n```yaml\nsummary: x\n```\n";
const PROPOSAL: &str = "# the current pattern\n```yaml\nsummary: x\n```\nQuote the source verbatim. GROUNDED-QUOTES\n";
const JUDGE: &str = "gemini-2.5-pro";

/// A staging root holding `traces` (article kind, the shared short source)
/// and a repo patterns dir holding `distill-article.md` = [`BASE`].
struct Repo {
    root: tempfile::TempDir,
}

impl Repo {
    fn new(traces: &[&str]) -> Self {
        let root = tempfile::tempdir().unwrap();
        for trace in traces {
            let dir = root.path().join("stages").join(trace);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
            std::fs::write(
                dir.join("distilled.yml"),
                serde_yaml::to_string(&vault::distilled::Distilled::default()).unwrap(),
            )
            .unwrap();
            std::fs::write(dir.join("transcript.md"), SOURCE).unwrap();
        }
        std::fs::create_dir_all(root.path().join("patterns")).unwrap();
        std::fs::write(root.path().join("patterns/distill-article.md"), BASE).unwrap();
        Self { root }
    }

    fn staged(&self) -> PathBuf {
        self.root.path().join("stages")
    }

    fn patterns(&self) -> PathBuf {
        self.root.path().join("patterns")
    }

    fn target_text(&self) -> String {
        std::fs::read_to_string(self.patterns().join("distill-article.md")).unwrap()
    }
}

fn objective(score: f64) -> Objective {
    Objective {
        score,
        factors: crate::eval::candidate::Factors {
            quote_grounding: score,
            claim_support: 1.0,
            listicle_survival: 1.0,
        },
        claims_per_note: 3.0,
        enumeration_items_per_note: 2.0,
        floor_breaches: Vec::new(),
    }
}

fn record(text: &str) -> CandidateFile {
    CandidateFile {
        pattern: "distill-article".to_string(),
        run_id: crate::eval::candidate::run_id("distill-article", text),
        baseline_run_id: crate::eval::candidate::run_id("distill-article", BASE),
        base_hash: stable_hash(BASE),
        candidate_hash: stable_hash(text),
        judge_model: JUDGE.to_string(),
        distill_model: "claude-sonnet-5".to_string(),
        created: "2026-10-17T12:00:00Z".to_string(),
        train: Comparison {
            traces: vec!["t1".to_string(), "t2".to_string()],
            baseline: objective(0.0),
            candidate: objective(1.0),
        },
        holdout: Comparison {
            traces: vec!["h1".to_string(), "h2".to_string()],
            baseline: objective(0.0),
            candidate: objective(1.0),
        },
        trust_at_write: Vec::new(),
        pattern_text: text.to_string(),
    }
}

fn trust_row(paired: f64, n: usize) -> TrustRow {
    TrustRow {
        judge_model: JUDGE.to_string(),
        check_name: CHECK_JUDGE_CLAIMS.to_string(),
        planted_class: "number-swap".to_string(),
        rubric_version: crate::eval::cache::CLAIMS_RUBRIC_VERSION.to_string(),
        paired_catch_rate: paired,
        raw_catch_rate: 0.96,
        flag_rate: 0.38,
        wilson_lower: 0.42,
        sample_size: n,
        measured_at: "2026-10-17T13:58:31Z".to_string(),
    }
}

async fn run_apply(repo: &Repo, rec: &CandidateFile, rows: &[TrustRow], distiller: &FakeDistiller) -> Result<Applied> {
    let env = env();
    let policy = TrustConfig::default();
    let target = repo.patterns();
    let staged = repo.staged();
    let req = ApplyRequest {
        target_dir: &target,
        staged_root: &staged,
        trust_rows: rows,
        policy: &policy,
        max_coverage_cut: 0.10,
        distill_workers: 2,
        distill_model: "claude-sonnet-5",
        scored_at: "2026-10-17T14:00:00Z",
    };
    apply(rec, &req, distiller, &env.grading()).await
}

#[tokio::test]
async fn apply_refuses_an_untrusted_claim_judge_naming_it_and_touches_nothing() {
    let repo = Repo::new(&["h1", "h2"]);
    let distiller = FakeDistiller::default();
    // The measured Gemini judge-claims trust: paired 0.58 over 26 number swaps.
    let err = run_apply(&repo, &record(PROPOSAL), &[trust_row(0.58, 26)], &distiller)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    for needle in [
        "claim-support",
        "judge-claims",
        JUDGE,
        "number-swap: n 26 < min-samples 30",
        "paired catch 0.58 < 0.80",
    ] {
        assert!(msg.contains(needle), "{needle} missing from: {msg}");
    }
    assert_eq!(
        distiller.calls.load(Ordering::SeqCst),
        0,
        "the gate runs before any distill"
    );
    assert_eq!(repo.target_text(), BASE);
}

#[tokio::test]
async fn apply_refuses_a_claim_judge_that_was_never_measured() {
    let repo = Repo::new(&["h1", "h2"]);
    let err = run_apply(&repo, &record(PROPOSAL), &[], &FakeDistiller::default())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("never measured"), "{err:#}");
    assert_eq!(repo.target_text(), BASE);
}

#[tokio::test]
async fn a_trusted_claim_judge_and_a_confirmed_holdout_write_the_pattern() {
    let repo = Repo::new(&["h1", "h2"]);
    let distiller = FakeDistiller::default();
    let applied = run_apply(&repo, &record(PROPOSAL), &[trust_row(0.80, 30)], &distiller)
        .await
        .unwrap();
    assert_eq!(repo.target_text(), PROPOSAL);
    assert_eq!(applied.target, repo.patterns().join("distill-article.md"));
    assert_eq!(
        distiller.calls.load(Ordering::SeqCst),
        4,
        "a fresh re-distill of both arms"
    );
    assert!(applied.holdout.candidate_wins());
    assert!(
        !repo.patterns().join(".distill-article.md.candidate").exists(),
        "the staging file is renamed into place"
    );
}

#[tokio::test]
async fn apply_refuses_when_the_fresh_holdout_does_not_confirm() {
    let repo = Repo::new(&["h1", "h2"]);
    // No GROUNDED-QUOTES: the proposal distills exactly like the baseline.
    let worse = "# a proposal that changes nothing measurable\n";
    let err = run_apply(&repo, &record(worse), &[trust_row(0.80, 30)], &FakeDistiller::default())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("does not confirm"), "{err:#}");
    assert_eq!(repo.target_text(), BASE);
}

#[tokio::test]
async fn apply_refuses_a_pattern_that_changed_since_the_measurement() {
    let repo = Repo::new(&["h1", "h2"]);
    std::fs::write(repo.patterns().join("distill-article.md"), "# edited by hand\n").unwrap();
    let err = run_apply(
        &repo,
        &record(PROPOSAL),
        &[trust_row(0.80, 30)],
        &FakeDistiller::default(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("the pattern changed"), "{err:#}");
}

#[tokio::test]
async fn apply_refuses_a_holdout_trace_swept_from_staging() {
    let repo = Repo::new(&["h1"]);
    let err = run_apply(
        &repo,
        &record(PROPOSAL),
        &[trust_row(0.80, 30)],
        &FakeDistiller::default(),
    )
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("h2 is no longer staged"), "{err:#}");
    assert_eq!(repo.target_text(), BASE);
}

#[test]
fn factor_trust_gates_only_the_judge_factor() {
    let t = factor_trust(JUDGE, &[trust_row(0.58, 26)], &TrustConfig::default());
    let states: Vec<(&str, &str)> = t.iter().map(|f| (f.factor.as_str(), f.state.as_str())).collect();
    assert_eq!(
        states,
        vec![
            ("quote-grounding", "ungated"),
            ("claim-support", "untrusted"),
            ("listicle-survival", "ungated")
        ]
    );
    assert!(gate(&t).is_err());
    assert!(gate(&factor_trust(JUDGE, &[trust_row(0.80, 30)], &TrustConfig::default())).is_ok());
}

#[test]
fn a_candidate_file_round_trips_and_a_hand_edited_pattern_is_refused() {
    let rec = record(PROPOSAL);
    let text = rec.render(BASE).unwrap();
    assert!(text.contains("````diff\n"), "{text}");
    assert!(text.contains("+Quote the source verbatim. GROUNDED-QUOTES"), "{text}");
    assert!(text.contains("| holdout (2) | candidate | 1.0000 |"), "{text}");
    assert_eq!(CandidateFile::parse(&text).unwrap(), rec);

    let tampered = text.replacen("GROUNDED-QUOTES", "GROUNDED-QUOTES and more", 1);
    let err = CandidateFile::parse(&tampered).unwrap_err();
    assert!(
        format!("{err:#}").contains("not the pattern that was measured"),
        "{err:#}"
    );
}

#[test]
fn a_candidate_file_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let rec = record(PROPOSAL);
    let first = rec.write(dir.path(), BASE).unwrap();
    let second = rec.write(dir.path(), BASE).unwrap();
    assert_eq!(first.file_name().unwrap(), "distill-article-2026-10-17.md");
    assert_eq!(second.file_name().unwrap(), "distill-article-2026-10-17-2.md");
}
