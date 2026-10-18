use super::*;

#[test]
fn parse_clean_yaml_reply() {
    let reply = "claim-coverage: 2\nanchor-validity: 3\nsummary-faithfulness: 1\n";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, 2);
    assert_eq!(s.anchor_validity, 3);
    assert_eq!(s.summary_faithfulness, 1);
}

#[test]
fn parse_tolerates_fences_prose_and_underscores() {
    let reply = "Here is my assessment:\n\n```yaml\nclaim_coverage = 3\nAnchor Validity: 2\nsummary-faithfulness:  0\n```\nDone.";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, 3);
    assert_eq!(s.anchor_validity, 2);
    assert_eq!(s.summary_faithfulness, 0);
}

#[test]
fn parse_clamps_out_of_range_scores() {
    let reply = "claim-coverage: 9\nanchor-validity: 4\nsummary-faithfulness: 2";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, MAX_SCORE);
    assert_eq!(s.anchor_validity, MAX_SCORE);
    assert_eq!(s.summary_faithfulness, 2);
}

#[test]
fn parse_errors_when_an_axis_is_missing() {
    let reply = "claim-coverage: 2\nanchor-validity: 3\n"; // no summary axis
    let err = parse_axis_scores(reply).expect_err("missing axis is an error");
    assert!(err.to_string().contains("summary"));
}

#[test]
fn composite_is_mean_of_axes() {
    let s = AxisScores {
        claim_coverage: 3,
        anchor_validity: 3,
        summary_faithfulness: 0,
    };
    assert!((s.composite() - 2.0).abs() < 1e-9);
}

#[test]
fn mock_judge_returns_per_kind_and_counts_calls() {
    let judge = MockJudge::new(AxisScores {
        claim_coverage: 1,
        anchor_validity: 1,
        summary_faithfulness: 1,
    })
    .with(
        "video",
        AxisScores {
            claim_coverage: 3,
            anchor_validity: 2,
            summary_faithfulness: 3,
        },
    );
    let v = judge.judge("video", "src", "note").expect("judge");
    assert_eq!(v.claim_coverage, 3);
    let a = judge.judge("article", "src", "note").expect("judge");
    assert_eq!(a.claim_coverage, 1); // falls back to default
    assert_eq!(judge.calls(), 2);
}

#[test]
fn parse_claim_verdicts_clean_reply() {
    let reply = "1: supported\n2: contradicted\n3: unsupported\n";
    let v = parse_claim_verdicts(reply, 3).expect("parse");
    assert_eq!(
        v,
        vec![
            ClaimVerdict::Supported,
            ClaimVerdict::Contradicted,
            ClaimVerdict::Unsupported
        ]
    );
}

#[test]
fn parse_claim_verdicts_tolerates_decoration() {
    let reply = "Verdicts:\n```\n- 1. **Supported**\n* 2) unsupported\n```";
    let v = parse_claim_verdicts(reply, 2).expect("parse");
    assert_eq!(v, vec![ClaimVerdict::Supported, ClaimVerdict::Unsupported]);
}

#[test]
fn parse_claim_verdicts_errors_on_missing_claim() {
    let err = parse_claim_verdicts("1: supported\n3: supported\n", 3).unwrap_err();
    assert!(err.to_string().contains("[2]"), "{err}");
}

#[test]
fn parse_claim_verdicts_ignores_out_of_range_and_duplicates() {
    let reply = "1: contradicted\n1: supported\n9: supported\n0: supported\n";
    let v = parse_claim_verdicts(reply, 1).expect("parse");
    assert_eq!(v, vec![ClaimVerdict::Contradicted]);
}

#[test]
fn claim_verdict_code_round_trips() {
    for v in [
        ClaimVerdict::Supported,
        ClaimVerdict::Unsupported,
        ClaimVerdict::Contradicted,
    ] {
        assert_eq!(ClaimVerdict::from_code(v.code()), Some(v));
    }
    assert_eq!(ClaimVerdict::from_code('x'), None);
}

#[test]
fn distillation_input_puts_the_note_before_the_source() {
    let input = distillation_input("video", "THE NOTE", "THE SOURCE");
    let note_at = input.find("THE NOTE").expect("note present");
    let source_at = input.find("THE SOURCE").expect("source present");
    assert!(note_at < source_at, "{input}");
}

#[test]
fn source_budget_is_what_the_note_and_framing_leave() {
    let note = "n".repeat(1_000);
    let budget = source_budget(24_000, "article", &note);
    let framed = distillation_input("article", &note, "").chars().count();
    assert_eq!(budget, 24_000 - framed);
    assert_eq!(
        distillation_input("article", &note, &"s".repeat(budget))
            .chars()
            .count(),
        24_000
    );
}

#[test]
fn source_budget_is_zero_when_the_note_fills_the_input() {
    assert_eq!(source_budget(100, "article", &"n".repeat(500)), 0);
}

/// The child env value `name` set on `cmd`, if any.
fn child_env(cmd: &Command, name: &str) -> Option<String> {
    cmd.get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new(name))
        .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
}

/// Both borg judges carry the configured
/// `llm.api-keys` map into the fabric child they run.
#[test]
fn both_judges_export_the_configured_key_map_on_their_fabric_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gemini = dir.path().join("gemini");
    let openai = dir.path().join("openai");
    std::fs::write(&gemini, "gemini-from-config").expect("write");
    std::fs::write(&openai, "openai-from-config").expect("write");
    let yaml = format!(
        "llm:\n  api-keys:\n    gemini: {}\n    openai: {}\n",
        gemini.display(),
        openai.display()
    );
    let config: crate::config::Config = serde_yaml::from_str(&yaml).expect("parse borg.yml");
    let opts = crate::eval::EvalOpts {
        judge_model: "gemini-2.5-flash".to_string(),
        api_keys: config.llm.api_keys.clone(),
        ..Default::default()
    };
    let (judge, claim_judge) = crate::eval::fabric_judges(&opts).expect("judges");
    assert_eq!(judge.api_keys, config.llm.api_keys);
    assert_eq!(claim_judge.api_keys, config.llm.api_keys);
    for cmd in [judge.command(), claim_judge.command()] {
        assert_eq!(child_env(&cmd, "GEMINI_API_KEY").as_deref(), Some("gemini-from-config"));
        assert_eq!(child_env(&cmd, "OPENAI_API_KEY").as_deref(), Some("openai-from-config"));
    }
}

/// Every LLM judge is built through `fabric_judges`, which refuses a blank
/// model for a judged run: no `-m` would judge with fabric's default model.
#[test]
fn fabric_judges_refuses_a_blank_model_for_a_judged_run() {
    for model in ["", "   "] {
        let opts = crate::eval::EvalOpts {
            judge_model: model.to_string(),
            ..Default::default()
        };
        let err = crate::eval::fabric_judges(&opts).unwrap_err().to_string();
        assert!(err.contains("no judge model"), "{model:?}: {err}");
    }
    // A run that judges nothing names no model and still builds (never called).
    let unjudged = crate::eval::EvalOpts {
        no_judge: true,
        ..Default::default()
    };
    crate::eval::fabric_judges(&unjudged).expect("--no-judge builds without a model");
    let named = crate::eval::EvalOpts {
        judge_model: "gemini-2.5-pro".to_string(),
        ..Default::default()
    };
    crate::eval::fabric_judges(&named).expect("a named model builds");
}

/// A stand-in fabric that writes the char count of its stdin to `count` and
/// answers one `supported` verdict.
fn counting_fabric(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let count = dir.join("count");
    let script = dir.join("fabric");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nwc -c > '{}'\necho '1: supported'\n", count.display()),
    )
    .expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (script, count)
}

/// `FabricClaimJudge` hands fabric the whole input (`max_chars` 0): a 300k
/// source arrives intact.
#[test]
fn fabric_claim_judge_sends_the_whole_source_to_fabric() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (script, count) = counting_fabric(dir.path());
    let mut judge = FabricClaimJudge::new("gemini-2.5-pro", ApiKeys::default());
    judge.binary = script.to_string_lossy().into_owned();
    let source = "s".repeat(300_000);
    let claims = vec!["a claim".to_string()];
    let verdicts = judge.judge_claims("article", &source, &claims).expect("judged");
    assert_eq!(verdicts, vec![ClaimVerdict::Supported]);
    let sent: usize = std::fs::read_to_string(&count)
        .expect("count")
        .trim()
        .parse()
        .expect("a number");
    assert_eq!(sent, claims_input("article", &claims, &source).len());
    assert!(sent > 300_000, "the source was trimmed: {sent} bytes sent");
}

/// Fail closed: a source over the judge's cap is refused, never trimmed and
/// never sent.
#[test]
fn fabric_claim_judge_refuses_a_source_over_its_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (script, count) = counting_fabric(dir.path());
    let mut judge = FabricClaimJudge::new("gemini-2.5-pro", ApiKeys::default());
    judge.binary = script.to_string_lossy().into_owned();
    judge.source_max_chars = 1_000;
    let err = judge
        .judge_claims("article", &"s".repeat(1_001), &["c".to_string()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("1001-char source over the 1000-char cap"), "{err}");
    assert!(!count.exists(), "fabric must not run on an over-cap source");
    judge
        .judge_claims("article", &"s".repeat(1_000), &["c".to_string()])
        .expect("a source at the cap is judged");
}

#[test]
fn claims_input_numbers_the_claims_before_the_source() {
    let input = claims_input("video", &["  first ".to_string(), "second".to_string()], "THE SOURCE");
    assert_eq!(
        input,
        "# KIND\nvideo\n\n# CLAIMS\n1. first\n2. second\n\n# SOURCE\nTHE SOURCE\n"
    );
}
