#![allow(clippy::unwrap_used)]

use super::*;
use clap::Parser;

// Wrap HarvestCliArgs in a minimal Parser so we can drive try_parse_from
// without standing up the entire sb CLI tree (same harness pattern as
// sb/src/cli/cortex/tests.rs).
#[derive(Parser)]
struct HarvestHarness {
    #[command(flatten)]
    harvest: HarvestCliArgs,
}

// One test, not three: the env-var leg mutates process environment, and a
// parallel sibling asserting the None default would race it. Sequential
// within a single test body, the order is the precedence chain itself.
#[test]
fn harvest_dormant_after_flag_env_default_precedence() {
    // Default: no flag, no env -> None (harvest::run falls back to config).
    let h = HarvestHarness::try_parse_from(["sb"]).unwrap();
    assert_eq!(h.harvest.dormant_after, None);

    // Flag parses.
    let h = HarvestHarness::try_parse_from(["sb", "--dormant-after", "1d"]).unwrap();
    assert_eq!(h.harvest.dormant_after.as_deref(), Some("1d"));

    // SAFETY: test-local env mutation; no other test reads this variable.
    unsafe { std::env::set_var("BORG_HARVEST_DORMANT_AFTER", "2d") };
    let from_env = HarvestHarness::try_parse_from(["sb"]).unwrap();
    assert_eq!(from_env.harvest.dormant_after.as_deref(), Some("2d"));
    // An explicit flag beats the env var.
    let from_flag = HarvestHarness::try_parse_from(["sb", "--dormant-after", "1d"]).unwrap();
    assert_eq!(from_flag.harvest.dormant_after.as_deref(), Some("1d"));
    unsafe { std::env::remove_var("BORG_HARVEST_DORMANT_AFTER") };
}

#[derive(Parser)]
struct EvalHarness {
    #[command(flatten)]
    eval: args::EvalArgs,
}

/// `sb borg eval` hands borg.yml's `llm.api-keys` to the
/// eval, so both judges see the configured map.
#[test]
fn eval_opts_carry_borg_yml_llm_api_keys() {
    let config: borg::config::Config =
        serde_yaml::from_str("llm:\n  api-key: ESCOTE_ANTHROPIC_API_KEY\n  api-keys:\n    gemini: GEMINI_API_KEY\n")
            .unwrap();
    let h = EvalHarness::try_parse_from(["sb", "--staged", "--no-judge"]).unwrap();
    let opts = eval_opts(&config, &h.eval).unwrap();
    assert_eq!(opts.api_keys, config.llm.api_keys);
    assert_eq!(
        opts.api_keys.get(vault::fabric::Vendor::Anthropic),
        Some("ESCOTE_ANTHROPIC_API_KEY")
    );
    assert_eq!(opts.api_keys.get(vault::fabric::Vendor::Gemini), Some("GEMINI_API_KEY"));
}

/// Gemini is the default judge: with no `--judge-model`, a judged run uses
/// the first of `eval.judge-models`, whose default is Gemini then Anthropic.
#[test]
fn the_default_judge_is_gemini_and_a_flag_or_config_overrides_it() {
    let defaults = borg::config::Config::default();
    assert_eq!(
        defaults.eval.judge_models,
        vec![borg::config::DEFAULT_JUDGE_MODEL, borg::config::SECOND_JUDGE_MODEL]
    );
    assert!(borg::config::DEFAULT_JUDGE_MODEL.starts_with("gemini-"));
    assert!(borg::config::SECOND_JUDGE_MODEL.starts_with("claude-"));

    let staged = EvalHarness::try_parse_from(["sb", "--staged"]).unwrap();
    assert_eq!(
        eval_opts(&defaults, &staged.eval).unwrap().judge_model,
        "gemini-2.5-pro"
    );

    let flagged = EvalHarness::try_parse_from(["sb", "--staged", "--judge-model", "gpt-4o"]).unwrap();
    assert_eq!(eval_opts(&defaults, &flagged.eval).unwrap().judge_model, "gpt-4o");

    let configured: borg::config::Config =
        serde_yaml::from_str("eval:\n  judge-models: [gemini-3.1-pro-preview, claude-opus-4-8]\n").unwrap();
    assert_eq!(
        eval_opts(&configured, &staged.eval).unwrap().judge_model,
        "gemini-3.1-pro-preview"
    );

    // A run that judges nothing names no judge.
    let unjudged = EvalHarness::try_parse_from(["sb", "--staged", "--no-judge"]).unwrap();
    assert_eq!(eval_opts(&defaults, &unjudged.eval).unwrap().judge_model, "");
    // An explicitly emptied list falls back to fabric's own default model.
    let emptied: borg::config::Config = serde_yaml::from_str("eval:\n  judge-models: []\n").unwrap();
    assert_eq!(eval_opts(&emptied, &staged.eval).unwrap().judge_model, "");
}

#[test]
fn eval_format_defaults_to_text_and_takes_json_in_any_case() {
    let h = EvalHarness::try_parse_from(["sb"]).unwrap();
    assert_eq!(h.eval.format, args::EvalFormat::Text);
    let h = EvalHarness::try_parse_from(["sb", "--format", "JSON"]).unwrap();
    assert_eq!(h.eval.format, args::EvalFormat::Json);
    assert!(EvalHarness::try_parse_from(["sb", "--format", "yaml"]).is_err());
    assert!(EvalHarness::try_parse_from(["sb", "--format", "json", "--regenerate"]).is_err());
}

#[test]
fn render_eval_jsonl_writes_one_parseable_line_per_trace() {
    let opts = borg::eval::EvalOpts {
        fixtures_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures"),
        no_judge: true,
        ..Default::default()
    };
    let fixtures = borg::eval::load(&opts.fixtures_dir).unwrap();
    let cache = borg::eval::cache::JudgmentCache::open_memory().unwrap();
    // --no-judge: the judges are never called.
    let (judge, claim_judge) = borg::eval::fabric_judges(&opts).unwrap();
    let outcome = borg::eval::evaluate(
        &fixtures,
        &std::collections::BTreeMap::new(),
        &cache,
        &judge,
        &claim_judge,
        &opts,
    )
    .unwrap();
    let borg::eval::EvalOutcome::Report(report) = outcome else {
        panic!("expected a report");
    };
    let jsonl = render_eval_jsonl(&report).unwrap();
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(lines.len(), fixtures.len());
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["run_kind"], "baseline");
    }
}

#[test]
fn eval_unscored_takes_limit_or_count_and_refuses_the_dev_run_flags() {
    let h = EvalHarness::try_parse_from(["sb", "--unscored", "--limit", "5"]).unwrap();
    assert!(h.eval.unscored);
    assert_eq!(h.eval.limit, Some(5));
    assert!(
        EvalHarness::try_parse_from(["sb", "--unscored", "--count"])
            .unwrap()
            .eval
            .count
    );
    assert!(EvalHarness::try_parse_from(["sb", "--unscored", "--count", "--limit", "5"]).is_err());
    assert!(
        EvalHarness::try_parse_from(["sb", "--limit", "5"]).is_err(),
        "--limit needs --unscored"
    );
    assert!(
        EvalHarness::try_parse_from(["sb", "--count"]).is_err(),
        "--count needs --unscored"
    );
    for flag in [
        &["--staged"][..],
        &["--no-judge"],
        &["--judge-model", "m"],
        &["--regenerate"],
        &["--sample", "3"],
    ] {
        let mut argv = vec!["sb", "--unscored"];
        argv.extend_from_slice(flag);
        assert!(EvalHarness::try_parse_from(argv).is_err(), "{flag:?} must conflict");
    }
}

#[test]
fn eval_inject_takes_a_sample_and_refuses_the_other_modes() {
    let h = EvalHarness::try_parse_from(["sb", "--inject", "--sample", "30"]).unwrap();
    assert!(h.eval.inject);
    assert_eq!(h.eval.sample, Some(30));
    assert_eq!(
        EvalHarness::try_parse_from(["sb", "--inject"]).unwrap().eval.sample,
        None,
        "the sample defaults to eval.trust.min-samples at run time"
    );
    assert!(
        EvalHarness::try_parse_from(["sb", "--staged", "--sample", "3"]).is_ok(),
        "--staged still takes --sample"
    );
    assert!(
        EvalHarness::try_parse_from(["sb", "--sample", "3"]).is_err(),
        "--sample needs --staged or --inject"
    );
    for flag in [
        &["--staged"][..],
        &["--unscored"],
        &["--no-judge"],
        &["--judge-model", "m"],
        &["--regenerate"],
        &["--format", "json"],
    ] {
        let mut argv = vec!["sb", "--inject"];
        argv.extend_from_slice(flag);
        assert!(EvalHarness::try_parse_from(argv).is_err(), "{flag:?} must conflict");
    }
}

#[test]
fn eval_scored_needs_a_limit_and_refuses_the_other_modes() {
    let h = EvalHarness::try_parse_from(["sb", "--scored", "--limit", "10", "--format", "json"]).unwrap();
    assert!(h.eval.scored);
    assert_eq!(h.eval.limit, Some(10));
    assert!(
        EvalHarness::try_parse_from(["sb", "--scored"]).is_err(),
        "an unbounded export would send every scored trace to a paid judge"
    );
    for flag in [
        &["--unscored"][..],
        &["--inject"],
        &["--staged"],
        &["--no-judge"],
        &["--ingest-verdicts", "v.jsonl"],
    ] {
        let mut argv = vec!["sb", "--scored", "--limit", "3"];
        argv.extend_from_slice(flag);
        assert!(EvalHarness::try_parse_from(argv).is_err(), "{flag:?} must conflict");
    }
}

#[test]
fn eval_ingest_verdicts_takes_a_file_and_refuses_the_other_modes() {
    let h = EvalHarness::try_parse_from(["sb", "--ingest-verdicts", "v.jsonl"]).unwrap();
    assert_eq!(h.eval.ingest_verdicts, Some(std::path::PathBuf::from("v.jsonl")));
    for flag in [
        &["--unscored"][..],
        &["--scored", "--limit", "3"],
        &["--inject"],
        &["--staged"],
        &["--limit", "3"],
        &["--format", "json"],
    ] {
        let mut argv = vec!["sb", "--ingest-verdicts", "v.jsonl"];
        argv.extend_from_slice(flag);
        assert!(EvalHarness::try_parse_from(argv).is_err(), "{flag:?} must conflict");
    }
}

#[test]
fn remediate_is_a_dry_run_unless_apply_and_apply_needs_remediate() {
    let dry = EvalHarness::try_parse_from(["sb", "--remediate"]).unwrap();
    assert!(dry.eval.remediate && !dry.eval.apply);
    let applied = EvalHarness::try_parse_from(["sb", "--remediate", "--apply"]).unwrap();
    assert!(applied.eval.apply);
    assert!(EvalHarness::try_parse_from(["sb", "--apply"]).is_err());
    assert!(EvalHarness::try_parse_from(["sb", "--remediate", "--unscored"]).is_err());
    assert!(EvalHarness::try_parse_from(["sb", "--remediate", "--judge-model", "x"]).is_err());
}
