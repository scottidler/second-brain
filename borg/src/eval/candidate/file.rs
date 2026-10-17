//! The candidate file and `sb borg eval --apply-candidate`.
//!
//! A candidate file (`<report-dir>/candidates/<pattern>-<date>.md`) is a
//! markdown page for a human (before/after metrics on both samples, each
//! metric factor's trust state, the diff) over a YAML frontmatter that is the
//! machine record: the pattern, both run ids, the hash of the pattern it was
//! measured against, both trace lists, both samples' objectives, and the full
//! candidate text.
//!
//! `--apply-candidate` reads that record and refuses, in order, when: a metric
//! factor comes from a judge check `--inject` has not measured trusted; the
//! repo pattern is no longer the one the candidate was measured against; a
//! holdout trace is gone from staging; or a fresh re-distill of the holdout
//! sample does not beat the baseline with the floor intact. Only then is the
//! pattern written, for the normal ship (commit, `bump release`,
//! `otto deploy`).

use std::path::{Path, PathBuf};

use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{
    Arm, Grading, Objective, PatternDistiller, SampleScore, ScoreRequest, load_traces, render_objective, score,
};
use crate::config::TrustConfig;
use crate::eval::CHECK_JUDGE_CLAIMS;
use crate::eval::cache::stable_hash;
use crate::eval::scores::{CHECK_LISTICLE_SURVIVAL, CHECK_QUOTE_GROUNDING, NO_MODEL};
use crate::eval::trust::{TrustRow, TrustState, judge_check_trust};

/// The frontmatter fence.
const FENCE: &str = "---";

/// One metric factor, the check that produces it, and that check's trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FactorTrust {
    pub factor: String,
    pub check: String,
    /// `""` for a deterministic check.
    pub judge_model: String,
    /// `ungated`, `trusted` or `untrusted`.
    pub state: String,
    /// Per failing class: `<class>: <why>`. Empty unless untrusted.
    pub reasons: Vec<String>,
}

impl FactorTrust {
    pub fn may_act(&self) -> bool {
        self.state != "untrusted"
    }
}

/// The trust state of each metric factor's check: quote grounding and
/// listicle survival are deterministic (ungated); claim support is
/// `judge-claims` under `judge_model`, gated by the trust rule.
pub fn factor_trust(judge_model: &str, rows: &[TrustRow], policy: &TrustConfig) -> Vec<FactorTrust> {
    [
        ("quote-grounding", CHECK_QUOTE_GROUNDING, NO_MODEL),
        ("claim-support", CHECK_JUDGE_CLAIMS, judge_model),
        ("listicle-survival", CHECK_LISTICLE_SURVIVAL, NO_MODEL),
    ]
    .into_iter()
    .map(|(factor, check, model)| {
        let (state, reasons) = match judge_check_trust(model, check, rows, policy) {
            TrustState::Ungated => ("ungated", Vec::new()),
            TrustState::Trusted => ("trusted", Vec::new()),
            TrustState::Untrusted(why) => (
                "untrusted",
                why.iter().map(|w| format!("{}: {}", w.class, w.reason)).collect(),
            ),
        };
        FactorTrust {
            factor: factor.to_string(),
            check: check.to_string(),
            judge_model: model.to_string(),
            state: state.to_string(),
            reasons,
        }
    })
    .collect()
}

/// Refuse when any factor's check may not act, naming each one. Dropping the
/// factor instead would silently change the objective.
pub fn gate(trust: &[FactorTrust]) -> Result<()> {
    let blocked: Vec<String> = trust
        .iter()
        .filter(|t| !t.may_act())
        .map(|t| {
            format!(
                "{} comes from {} under {}, which is untrusted ({})",
                t.factor,
                t.check,
                t.judge_model,
                t.reasons.join("; ")
            )
        })
        .collect();
    if !blocked.is_empty() {
        bail!(
            "refusing the candidate: {}. `sb borg eval --inject` must measure the check trusted first",
            blocked.join(" | ")
        );
    }
    Ok(())
}

/// Both arms' objectives on one sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Comparison {
    pub traces: Vec<String>,
    pub baseline: Objective,
    pub candidate: Objective,
}

impl Comparison {
    fn of(sample: &SampleScore) -> Result<Self> {
        match (&sample.baseline, &sample.candidate) {
            (Some(baseline), Some(candidate)) => Ok(Self {
                traces: sample.traces.iter().map(|t| t.trace.clone()).collect(),
                baseline: baseline.clone(),
                candidate: candidate.clone(),
            }),
            _ => {
                let errored: Vec<String> = sample
                    .traces
                    .iter()
                    .filter(|t| t.score.is_none())
                    .map(|t| format!("{} ({})", t.trace, t.errors.join("; ")))
                    .collect();
                bail!("the sample is incomplete, so no candidate file: {}", errored.join(", "))
            }
        }
    }

    /// Does the candidate beat the baseline, floor intact?
    pub fn candidate_wins(&self) -> bool {
        self.candidate.floor_breaches.is_empty() && self.candidate.score > self.baseline.score
    }
}

/// The machine record in a candidate file's frontmatter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CandidateFile {
    pub pattern: String,
    pub run_id: String,
    pub baseline_run_id: String,
    /// Hash of the pattern text the candidate was measured against: apply
    /// refuses when the repo pattern no longer hashes to it.
    pub base_hash: String,
    pub candidate_hash: String,
    pub judge_model: String,
    pub distill_model: String,
    /// RFC 3339.
    pub created: String,
    pub train: Comparison,
    pub holdout: Comparison,
    /// Each factor's trust when the file was written. Apply re-reads trust;
    /// this is the record.
    pub trust_at_write: Vec<FactorTrust>,
    pub pattern_text: String,
}

/// Everything [`CandidateFile::build`] reads.
pub struct BuildInput<'a> {
    pub pattern: &'a str,
    pub baseline: &'a Arm,
    pub candidate: &'a Arm,
    pub distill_model: &'a str,
    pub created: &'a str,
    pub train: &'a SampleScore,
    pub holdout: &'a SampleScore,
    pub trust: Vec<FactorTrust>,
}

impl CandidateFile {
    /// The record for a winner measured on two disjoint, complete samples.
    pub fn build(input: BuildInput<'_>) -> Result<Self> {
        if input.baseline.text == input.candidate.text {
            bail!("the candidate is the baseline pattern; there is nothing to propose");
        }
        let train = Comparison::of(input.train)?;
        let holdout = Comparison::of(input.holdout)?;
        if let Some(shared) = train.traces.iter().find(|t| holdout.traces.contains(t)) {
            bail!("trace {shared} is in both samples; the holdout must be disjoint");
        }
        Ok(Self {
            pattern: input.pattern.to_string(),
            run_id: input.candidate.run_id.clone(),
            baseline_run_id: input.baseline.run_id.clone(),
            base_hash: stable_hash(&input.baseline.text),
            candidate_hash: stable_hash(&input.candidate.text),
            judge_model: input.train.judge_model.clone(),
            distill_model: input.distill_model.to_string(),
            created: input.created.to_string(),
            train,
            holdout,
            trust_at_write: input.trust,
            pattern_text: input.candidate.text.clone(),
        })
    }

    /// The file: frontmatter, then the human page. `base_text` is the
    /// pattern the candidate was measured against, for the diff.
    pub fn render(&self, base_text: &str) -> Result<String> {
        let yaml = serde_yaml::to_string(self).context("serializing the candidate record")?;
        let mut o = format!("{FENCE}\n{yaml}{FENCE}\n\n");
        let date = self.created.get(..10).unwrap_or(&self.created);
        o.push_str(&format!("# Candidate: {} ({date})\n\n", self.pattern));
        o.push_str(&format!(
            "Measured against `{}` (`{}`), judge `{}`, distiller `{}`, on two disjoint staged samples.\n\n",
            self.baseline_run_id, self.pattern, self.judge_model, self.distill_model
        ));
        o.push_str("| sample | arm | score | quote grounding | claim support | listicle survival | claims/note | items/note |\n");
        o.push_str("|---|---|---|---|---|---|---|---|\n");
        for (label, c) in [("train", &self.train), ("holdout", &self.holdout)] {
            for (arm, obj) in [("baseline", &c.baseline), ("candidate", &c.candidate)] {
                o.push_str(&format!(
                    "| {label} ({}) | {arm} | {:.4} | {:.3} | {:.3} | {:.3} | {:.2} | {:.2} |\n",
                    c.traces.len(),
                    obj.score,
                    obj.factors.quote_grounding,
                    obj.factors.claim_support,
                    obj.factors.listicle_survival,
                    obj.claims_per_note,
                    obj.enumeration_items_per_note
                ));
            }
        }
        for (label, c) in [("train", &self.train), ("holdout", &self.holdout)] {
            o.push_str(&format!(
                "\n{label}: the candidate {} the baseline ({:.4} vs {:.4}).",
                if c.candidate_wins() { "beats" } else { "does not beat" },
                c.candidate.score,
                c.baseline.score
            ));
            for b in &c.candidate.floor_breaches {
                o.push_str(&format!(" Coverage floor: {b}."));
            }
        }
        o.push_str(&format!(
            "\n\ntrain traces: {}\nholdout traces: {}\n",
            self.train.traces.join(" "),
            self.holdout.traces.join(" ")
        ));
        o.push_str("\n## Trust\n\n");
        for t in &self.trust_at_write {
            let who = if t.judge_model.is_empty() {
                "deterministic".to_string()
            } else {
                t.judge_model.clone()
            };
            o.push_str(&format!("- {}: `{}` ({who}) {}", t.factor, t.check, t.state));
            if !t.reasons.is_empty() {
                o.push_str(&format!(" ({})", t.reasons.join("; ")));
            }
            o.push('\n');
        }
        let blocked = self.trust_at_write.iter().any(|t| !t.may_act());
        o.push_str("\n## Apply\n\n");
        o.push_str(&format!(
            "From the repo root: `sb borg eval --apply-candidate <this file>`. It refuses while a factor's check is \
             untrusted, re-distills the holdout sample to confirm the win, then writes `borg/patterns/{}.md` for the \
             normal ship (commit, `bump release`, `otto deploy`).\n\n",
            self.pattern
        ));
        o.push_str(if blocked {
            "Status when written: BLOCKED (an untrusted judge check feeds the metric).\n"
        } else if !self.holdout.candidate_wins() {
            "Status when written: BLOCKED (the candidate does not beat the baseline on the holdout sample).\n"
        } else {
            "Status when written: READY.\n"
        });
        let diff = similar::TextDiff::from_lines(base_text, &self.pattern_text);
        let unified = diff
            .unified_diff()
            .context_radius(3)
            .header(
                &format!("a/borg/patterns/{}.md", self.pattern),
                &format!("b/borg/patterns/{}.md", self.pattern),
            )
            .to_string();
        // Four backticks: the pattern's own ``` fences stay inside the block.
        o.push_str(&format!("\n## Diff\n\n````diff\n{unified}````\n"));
        Ok(o)
    }

    /// Read a candidate file back. The candidate text must hash to the
    /// recorded `candidate-hash`: a hand-edited pattern is not the one that
    /// was measured.
    pub fn parse(text: &str) -> Result<Self> {
        let rest = text
            .strip_prefix(&format!("{FENCE}\n"))
            .ok_or_else(|| eyre::eyre!("a candidate file starts with a `---` frontmatter fence"))?;
        let end = rest
            .find(&format!("\n{FENCE}\n"))
            .ok_or_else(|| eyre::eyre!("the candidate frontmatter has no closing `---`"))?;
        let record: Self = serde_yaml::from_str(&rest[..=end]).context("parsing the candidate frontmatter")?;
        let hash = stable_hash(&record.pattern_text);
        if hash != record.candidate_hash {
            bail!(
                "the candidate text hashes to {hash}, not the recorded {}: it is not the pattern that was measured",
                record.candidate_hash
            );
        }
        if super::run_id(&record.pattern, &record.pattern_text) != record.run_id {
            bail!("run-id {} does not match the pattern and its text", record.run_id);
        }
        Ok(record)
    }

    /// Write the file into `dir` as `<pattern>-<date>.md`, `-2`, `-3`, ... on
    /// a collision: a candidate file is never overwritten.
    pub fn write(&self, dir: &Path, base_text: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let date = self.created.get(..10).unwrap_or(&self.created);
        let text = self.render(base_text)?;
        for n in 1usize.. {
            let name = if n == 1 {
                format!("{}-{date}.md", self.pattern)
            } else {
                format!("{}-{date}-{n}.md", self.pattern)
            };
            let path = dir.join(name);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(text.as_bytes())
                        .with_context(|| format!("writing {}", path.display()))?;
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
            }
        }
        unreachable!("1.. never ends")
    }
}

/// Everything `--apply-candidate` reads beyond the candidate record.
pub struct ApplyRequest<'a> {
    /// The repo patterns dir the candidate is written into (`borg/patterns`).
    pub target_dir: &'a Path,
    pub staged_root: &'a Path,
    pub trust_rows: &'a [TrustRow],
    pub policy: &'a TrustConfig,
    pub max_coverage_cut: f64,
    pub distill_workers: usize,
    pub distill_model: &'a str,
    pub scored_at: &'a str,
}

/// What an applied candidate did.
#[derive(Debug)]
pub struct Applied {
    pub target: PathBuf,
    /// The fresh holdout re-score that confirmed it.
    pub holdout: SampleScore,
}

impl Applied {
    pub fn render(&self) -> String {
        format!(
            "{}\nwrote {}\nship it: commit, `bump release`, `otto deploy`\n",
            self.holdout.render(),
            self.target.display()
        )
    }
}

/// Apply `record` (see the module docs for the refusal order). `g.opts` must
/// judge with `record.judge_model`, the judge the candidate was measured with.
pub async fn apply(
    record: &CandidateFile,
    req: &ApplyRequest<'_>,
    distiller: &dyn PatternDistiller,
    g: &Grading<'_>,
) -> Result<Applied> {
    log::debug!(
        "eval::candidate::file::apply: pattern={} run_id={} target_dir={}",
        record.pattern,
        record.run_id,
        req.target_dir.display()
    );
    gate(&factor_trust(&record.judge_model, req.trust_rows, req.policy))?;
    if g.opts.judge_model != record.judge_model {
        bail!(
            "the candidate was measured with judge {}, not {}",
            record.judge_model,
            g.opts.judge_model
        );
    }
    let target = req.target_dir.join(format!("{}.md", record.pattern));
    let baseline = Arm::load(&record.pattern, &target)?;
    let current = stable_hash(&baseline.text);
    if current != record.base_hash {
        bail!(
            "{} hashes to {current}, not {}: the pattern changed since the candidate was measured; run the search again",
            target.display(),
            record.base_hash
        );
    }
    let fixtures = load_traces(req.staged_root, &record.pattern, &record.holdout.traces)?;
    let dir = tempfile::tempdir().context("creating a temp dir for the candidate pattern")?;
    let candidate_file = dir.path().join(format!("{}.md", record.pattern));
    std::fs::write(&candidate_file, &record.pattern_text)
        .with_context(|| format!("writing {}", candidate_file.display()))?;
    let candidate = Arm::load(&record.pattern, &candidate_file)?;
    let holdout = score(
        &ScoreRequest {
            pattern: &record.pattern,
            baseline: &baseline,
            candidate: &candidate,
            fixtures: &fixtures,
            distill_model: req.distill_model,
            max_coverage_cut: req.max_coverage_cut,
            distill_workers: req.distill_workers,
            fresh: true,
            scored_at: req.scored_at,
        },
        distiller,
        g,
    )
    .await?;
    if !holdout.candidate_wins() {
        let line = |o: &Option<Objective>| o.as_ref().map_or("incomplete".to_string(), render_objective);
        bail!(
            "the fresh holdout re-score does not confirm the candidate, nothing written:\n  baseline  {}\n  candidate {}",
            line(&holdout.baseline),
            line(&holdout.candidate)
        );
    }
    let staging = req.target_dir.join(format!(".{}.md.candidate", record.pattern));
    std::fs::write(&staging, &record.pattern_text).with_context(|| format!("writing {}", staging.display()))?;
    std::fs::rename(&staging, &target).with_context(|| format!("replacing {}", target.display()))?;
    Ok(Applied { target, holdout })
}

#[cfg(test)]
mod tests;
