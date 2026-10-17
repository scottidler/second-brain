//! `sb borg eval --regenerate`: re-distill each capture-note fixture's
//! `source.md` through the current patterns, once without its capture note and
//! once with it, then grade both arms with the same metrics and judges as a
//! golden run.
//!
//! The two arms run over the same fixture set (those carrying a
//! `capture-note.txt`), so their numbers compare directly. Distilling is async
//! and goes through an injected [`distillers::Dispatch`]; grading stays the
//! sync [`evaluate`](crate::eval::evaluate), run after every distill has
//! returned, so the judges and the SQLite cache never live across an await.

use std::collections::BTreeMap;

use distillers::{Dispatch, DistillInputs, DistillKind};
use eyre::{Context, Result, bail};

use crate::eval::{
    ClaimJudge, DistillationJudge, EvalOpts, EvalOutcome, EvalReport, Fixture, cache, evaluate, fixtures,
    patterns::PatternDirFabric,
};

/// `run_id` of the arm distilled without its capture note.
pub const RUN_ID_WITHOUT_NOTE: &str = "regenerate:without-note";
/// `run_id` of the arm distilled with its capture note.
pub const RUN_ID_WITH_NOTE: &str = "regenerate:with-note";

/// Both graded arms of a `--regenerate` run.
#[derive(Debug, Clone)]
pub struct RegenerateReport {
    /// Distilled with no capture note.
    pub without_note: EvalReport,
    /// Distilled with the fixture's capture note.
    pub with_note: EvalReport,
    /// Fixture ids skipped because they carry no capture note.
    pub skipped_no_note: Vec<String>,
}

impl RegenerateReport {
    /// Every graded note of both arms as JSONL records (`--format json`):
    /// `run_kind` `candidate` (a re-distill under the current pattern dir),
    /// `run_id` `regenerate:without-note` or `regenerate:with-note`.
    pub fn records(&self) -> Vec<crate::eval::TraceRecord> {
        [
            (RUN_ID_WITHOUT_NOTE, &self.without_note),
            (RUN_ID_WITH_NOTE, &self.with_note),
        ]
        .into_iter()
        .flat_map(|(run_id, arm)| {
            arm.traces.iter().cloned().map(move |mut record| {
                record.run_kind = crate::eval::RUN_KIND_CANDIDATE.to_string();
                record.run_id = run_id.to_string();
                record
            })
        })
        .collect()
    }

    /// Plain-text report: one header, then each arm's full eval report.
    pub fn render(&self) -> String {
        format!(
            "regenerate: {} fixtures with a capture note, each distilled without and with it ({} without a note skipped)\n\n\
             === without capture note ===\n{}\n=== with capture note ===\n{}",
            self.with_note.total_fixtures,
            self.skipped_no_note.len(),
            self.without_note.render(),
            self.with_note.render(),
        )
    }
}

/// The distiller a fixture of this kind label goes through. An unknown label is
/// an error: a fixture the harness cannot route is never silently dropped.
pub fn distill_kind_for_fixture(kind: &str) -> Result<DistillKind> {
    Ok(match kind {
        "article" => DistillKind::Article,
        "repo" => DistillKind::Repo,
        "video" => DistillKind::Video,
        "thread" => DistillKind::Thread,
        "session" => DistillKind::Session,
        "image" => DistillKind::Image,
        "voicenote" => DistillKind::VoiceNote,
        "idea" => DistillKind::Idea,
        other => bail!("no distiller for fixture kind '{other}'"),
    })
}

/// The two re-distilled fixture sets, before grading.
#[derive(Debug, Clone)]
pub struct Regenerated {
    pub without_note: Vec<Fixture>,
    pub with_note: Vec<Fixture>,
    pub skipped_no_note: Vec<String>,
}

/// Distill every fixture that carries a capture note twice: without the note,
/// then with it. Fails when no fixture carries a note (there is nothing to
/// compare) or when any distill fails.
pub async fn redistill(fixtures: &[Fixture], dispatch: &dyn Dispatch) -> Result<Regenerated> {
    log::debug!("eval::regenerate::redistill: fixtures={}", fixtures.len());
    let mut out = Regenerated {
        without_note: Vec::new(),
        with_note: Vec::new(),
        skipped_no_note: Vec::new(),
    };
    for fx in fixtures {
        let Some(note) = fx.capture_note.as_deref() else {
            out.skipped_no_note.push(fx.id.clone());
            continue;
        };
        let kind = distill_kind_for_fixture(&fx.kind)?;
        let bare = dispatch
            .distill(
                kind,
                DistillInputs {
                    transcript: &fx.source,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("re-distilling fixture {} without its capture note", fx.id))?;
        let noted = dispatch
            .distill(
                kind,
                DistillInputs {
                    transcript: &fx.source,
                    capture_note: Some(note),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("re-distilling fixture {} with its capture note", fx.id))?;
        out.without_note.push(Fixture {
            distilled: bare,
            ..fx.clone()
        });
        out.with_note.push(Fixture {
            distilled: noted,
            ..fx.clone()
        });
    }
    if out.with_note.is_empty() {
        bail!(
            "no fixture carries a {}; run `sb borg eval --synth-intents` first",
            fixtures::CAPTURE_NOTE_FILE
        );
    }
    log::debug!(
        "eval::regenerate::redistill: regenerated={} skipped_no_note={}",
        out.with_note.len(),
        out.skipped_no_note.len()
    );
    Ok(out)
}

/// Grade both arms. Hand calibration labels describe the golden distillations,
/// not these, so both arms run uncalibrated.
pub fn grade(
    regenerated: &Regenerated,
    cache: &cache::JudgmentCache,
    judge: &dyn DistillationJudge,
    claim_judge: &dyn ClaimJudge,
    opts: &EvalOpts,
) -> Result<RegenerateReport> {
    if opts.emit_calibration.is_some() {
        bail!("--emit-calibration does not apply to --regenerate");
    }
    let no_labels = BTreeMap::new();
    let arm = |set: &[Fixture]| -> Result<EvalReport> {
        match evaluate(set, &no_labels, cache, judge, claim_judge, opts)? {
            EvalOutcome::Report(r) => Ok(*r),
            EvalOutcome::CalibrationSheet(_) => bail!("--regenerate produced a calibration sheet"),
        }
    };
    Ok(RegenerateReport {
        without_note: arm(&regenerated.without_note)?,
        with_note: arm(&regenerated.with_note)?,
        skipped_no_note: regenerated.skipped_no_note.clone(),
    })
}

/// The production dispatcher for `--regenerate`: borg's fabric-config
/// dispatcher with every distill pattern resolved from `opts.patterns_dir`
/// when set (deployed patterns otherwise).
pub fn regenerate_dispatcher(
    fabric: &crate::config::FabricConfig,
    opts: &EvalOpts,
) -> distillers::Dispatcher<PatternDirFabric<distillers::FabricShell>> {
    crate::stages::distill::dispatcher_with_caller(
        fabric,
        PatternDirFabric {
            inner: crate::stages::distill::fabric_shell(fabric),
            dir: opts.patterns_dir.clone(),
        },
    )
}

/// Run `--regenerate` end to end against the golden fixtures, distilling
/// through `dispatch` (production: borg's fabric-config dispatcher) and grading
/// with the fabric judges.
pub async fn run_regenerate(opts: &EvalOpts, dispatch: &dyn Dispatch) -> Result<RegenerateReport> {
    log::debug!(
        "eval::regenerate::run_regenerate: fixtures_dir={} no_judge={}",
        opts.fixtures_dir.display(),
        opts.no_judge
    );
    if opts.staged_root.is_some() {
        bail!("--regenerate re-distills the golden fixtures; drop --staged");
    }
    let fixtures = fixtures::load(&opts.fixtures_dir)?;
    let regenerated = redistill(&fixtures, dispatch).await?;
    let cache = cache::JudgmentCache::open(&vault::paths::borg_eval_cache_path())?;
    let (judge, claim_judge) = crate::eval::fabric_judges(opts)?;
    grade(&regenerated, &cache, &judge, &claim_judge, opts)
}

#[cfg(test)]
mod tests;
