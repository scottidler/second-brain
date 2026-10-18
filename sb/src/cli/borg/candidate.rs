//! `sb borg eval --candidate-split | --candidate-pattern | --apply-candidate`:
//! the CLI shell over `borg::eval::candidate`. The search that proposes
//! candidates is `python -m propose` in the `eval/` uv project (`otto gepa`).

use std::path::{Path, PathBuf};

use eyre::{Context, Result};

use borg::eval::candidate::{self, Arm, DistillCache, FabricPatternDistiller, Grading, ScoreRequest, file};

use super::args;

/// The distill cache key's model: the distiller's fabric model, or fabric's
/// own default when borg.yml leaves it empty (no `-m` reaches fabric then).
fn distill_model(config: &borg::config::Config) -> String {
    if config.fabric.model.is_empty() {
        "<fabric default>".to_string()
    } else {
        config.fabric.model.clone()
    }
}

fn pattern_of(a: &args::EvalArgs) -> Result<&str> {
    a.pattern.as_deref().ok_or_else(|| eyre::eyre!("--pattern is required"))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Refuse a prompt-candidate search while a factor of its score comes from an
/// untrusted judge check: the same gate as `--apply-candidate`, applied at the
/// search's first call so an untrusted judge never steers a search at all.
pub(super) fn search_gate(
    judge_model: &str,
    trust_rows: &[borg::eval::trust::TrustRow],
    policy: &borg::config::TrustConfig,
) -> Result<()> {
    file::gate(&file::factor_trust(judge_model, trust_rows, policy))
        .context("refusing the candidate search (python -m propose starts with --candidate-split)")
}

/// The `--candidate-split` JSON: the split plus `fabric-env`, fabric's var
/// name for each vendor in borg.yml's `llm.api-keys` mapped to its configured
/// reference (an env var name or a file path, never a value), so the search's
/// reflection call exports the same keys the Rust judges do.
pub(super) fn split_json(split: &candidate::Split, keys: &vault::fabric::ApiKeys) -> Result<serde_json::Value> {
    let mut json = serde_json::to_value(split).context("serializing the split")?;
    let fabric_env: serde_json::Map<String, serde_json::Value> = keys
        .iter()
        .filter(|(_, reference)| !reference.is_empty())
        .map(|(vendor, reference)| (vendor.fabric_env().to_string(), reference.into()))
        .collect();
    json["fabric-env"] = serde_json::Value::Object(fabric_env);
    Ok(json)
}

/// `--candidate-split N --pattern P`: the frozen train/holdout pair, as JSON.
/// Gated on judge trust: this is the search's first call.
pub(super) fn run_split(
    config: &borg::config::Config,
    opts: &borg::eval::EvalOpts,
    a: &args::EvalArgs,
    n: usize,
) -> Result<()> {
    let trust_rows = borg::eval::trust::TrustStore::open(&vault::paths::borg_eval_cache_path())?.all()?;
    search_gate(&opts.judge_model, &trust_rows, &config.eval.trust)?;
    let split = candidate::load_split(&config.staging.root, pattern_of(a)?, n, opts.claims_source_max_chars)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&split_json(&split, &config.llm.api_keys)?).context("serializing the split")?
    );
    Ok(())
}

/// The baseline pattern file: `--patterns-dir` when given, else the deployed
/// patterns dir.
fn baseline_file(opts: &borg::eval::EvalOpts, pattern: &str) -> PathBuf {
    opts.patterns_dir
        .clone()
        .unwrap_or_else(vault::paths::patterns_dir)
        .join(format!("{pattern}.md"))
}

/// `--candidate-pattern FILE --pattern P --traces ...`: score FILE against
/// the current pattern; with `--write-candidate`, also re-score on
/// `--holdout` and write the candidate file.
pub(super) async fn run_score(
    config: &borg::config::Config,
    opts: &borg::eval::EvalOpts,
    a: &args::EvalArgs,
    file_path: &Path,
) -> Result<()> {
    let pattern = pattern_of(a)?;
    borg::eval::backlog::preflight_keys(&opts.api_keys)?;
    let staged_root = config.staging.root.clone();
    let baseline = Arm::load(pattern, &baseline_file(opts, pattern))?;
    let proposal = Arm::load(pattern, &vault::paths::expand_tilde(file_path))?;
    let cache_path = vault::paths::borg_eval_cache_path();
    let judgments = borg::eval::cache::JudgmentCache::open(&cache_path)?;
    let distills = DistillCache::open(&cache_path)?;
    let scores = borg::eval::scores::ScoreStore::open(&cache_path)?;
    let (judge, claim_judge) = borg::eval::fabric_judges(opts)?;
    let grading = Grading {
        judge: &judge,
        claim_judge: &claim_judge,
        judgments: &judgments,
        distills: &distills,
        scores: &scores,
        opts,
    };
    let distiller = FabricPatternDistiller {
        fabric: config.fabric.clone(),
        patterns_dir: opts.patterns_dir.clone(),
        pattern: pattern.to_string(),
    };
    let model = distill_model(config);
    let scored_at = now();
    let train_fixtures = candidate::load_traces(&staged_root, pattern, &a.traces)?;
    let train_request = ScoreRequest {
        pattern,
        baseline: &baseline,
        candidate: &proposal,
        fixtures: &train_fixtures,
        distill_model: &model,
        max_coverage_cut: config.eval.candidate.max_coverage_cut,
        distill_workers: config.eval.candidate.distill_workers,
        fresh: false,
        scored_at: &scored_at,
    };
    let train = candidate::score(&train_request, &distiller, &grading).await?;

    let Some(dir) = &a.write_candidate else {
        match a.format {
            args::EvalFormat::Text => print!("{}", train.render()),
            args::EvalFormat::Json => println!(
                "{}",
                serde_json::to_string(&train).context("serializing the candidate score")?
            ),
        }
        if train.traces.is_empty() {
            eyre::bail!(
                "no trace was scored: all {} were over the {}-char claims cap (not judgeable): {}",
                train.dropped_over_cap.len(),
                opts.claims_source_max_chars,
                train.dropped_over_cap.join(", ")
            );
        }
        if train.traces.iter().all(|t| t.score.is_none()) {
            eyre::bail!("every trace errored on an arm; see the per-trace errors");
        }
        return Ok(());
    };

    let holdout_fixtures = candidate::load_traces(&staged_root, pattern, &a.holdout)?;
    let holdout_request = ScoreRequest {
        fixtures: &holdout_fixtures,
        ..train_request
    };
    let holdout = candidate::score(&holdout_request, &distiller, &grading).await?;
    let trust_rows = borg::eval::trust::TrustStore::open(&cache_path)?.all()?;
    let trust = file::factor_trust(&opts.judge_model, &trust_rows, &config.eval.trust);
    let record = file::CandidateFile::build(file::BuildInput {
        pattern,
        baseline: &baseline,
        candidate: &proposal,
        distill_model: &model,
        created: &scored_at,
        train: &train,
        holdout: &holdout,
        trust,
    })?;
    let dir = dir
        .as_ref()
        .map(vault::paths::expand_tilde)
        .unwrap_or_else(|| vault::paths::borg_eval_reports_dir().join("candidates"));
    let written = record.write(&dir, &baseline.text)?;
    match a.format {
        args::EvalFormat::Text => {
            println!("=== train ===\n{}", train.render());
            println!("=== holdout ===\n{}", holdout.render());
            println!("candidate file: {}", written.display());
        }
        args::EvalFormat::Json => println!(
            "{}",
            serde_json::json!({ "train": train, "holdout": holdout, "file": written })
        ),
    }
    Ok(())
}

/// `--apply-candidate FILE`: gate on judge trust, check the base pattern,
/// confirm on a fresh holdout re-distill, then write the pattern into
/// `--patterns-dir` (default the repo's `borg/patterns`).
pub(super) async fn run_apply(
    config: &borg::config::Config,
    opts: &borg::eval::EvalOpts,
    file_path: &Path,
) -> Result<()> {
    let path = vault::paths::expand_tilde(file_path);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let record = file::CandidateFile::parse(&text).with_context(|| format!("reading {}", path.display()))?;
    let target_dir = match &opts.patterns_dir {
        Some(dir) => dir.clone(),
        None => borg::eval::resolve_patterns_dir(Path::new("borg/patterns"))
            .context("--apply-candidate writes the repo's borg/patterns: run it from the repo root")?,
    };
    let cache_path = vault::paths::borg_eval_cache_path();
    let trust_rows = borg::eval::trust::TrustStore::open(&cache_path)?.all()?;
    // The trust gate runs inside `apply` before anything else; refuse here
    // too, before the key preflight, so an untrusted candidate never needs keys.
    file::gate(&file::factor_trust(
        &record.judge_model,
        &trust_rows,
        &config.eval.trust,
    ))?;
    let opts = borg::eval::EvalOpts {
        judge_model: record.judge_model.clone(),
        patterns_dir: Some(target_dir.clone()),
        no_judge: false,
        ..opts.clone()
    };
    borg::eval::backlog::preflight_keys(&opts.api_keys)?;
    let judgments = borg::eval::cache::JudgmentCache::open(&cache_path)?;
    let distills = DistillCache::open(&cache_path)?;
    let scores = borg::eval::scores::ScoreStore::open(&cache_path)?;
    let (judge, claim_judge) = borg::eval::fabric_judges(&opts)?;
    let grading = Grading {
        judge: &judge,
        claim_judge: &claim_judge,
        judgments: &judgments,
        distills: &distills,
        scores: &scores,
        opts: &opts,
    };
    let distiller = FabricPatternDistiller {
        fabric: config.fabric.clone(),
        patterns_dir: Some(target_dir.clone()),
        pattern: record.pattern.clone(),
    };
    let model = distill_model(config);
    let scored_at = now();
    let staged_root = config.staging.root.clone();
    let applied = file::apply(
        &record,
        &file::ApplyRequest {
            target_dir: &target_dir,
            staged_root: &staged_root,
            trust_rows: &trust_rows,
            policy: &config.eval.trust,
            max_coverage_cut: config.eval.candidate.max_coverage_cut,
            distill_workers: config.eval.candidate.distill_workers,
            distill_model: &model,
            scored_at: &scored_at,
        },
        &distiller,
        &grading,
    )
    .await?;
    print!("{}", applied.render());
    Ok(())
}
