//! Harvest entry point: scan ~/.claude/projects, parse each JSONL,
//! run tier-1 classify, upsert into the sessions table (or quarantine).
//!
//! Idempotent: a session whose `jsonl_sha256` matches the stored row
//! is skipped unless `opts.force` is set.

use eyre::{Context, Result};
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::classify::{self, ClassifyOutcome};
use crate::config::Config;
use crate::jsonl;
use crate::ledger::Ledger;
use crate::opts::HarvestOpts;
use crate::scan;
use crate::types::quarantine_reason;

#[derive(Debug, Default, Clone)]
pub struct HarvestReport {
    pub n_discovered: usize,
    pub n_classified: usize,
    pub n_skipped_unchanged: usize,
    pub n_quarantined: usize,
}

pub fn run(ledger: &Ledger, config: &Config, opts: &HarvestOpts) -> Result<HarvestReport> {
    log::info!(
        "harvest::run: projects_dir={} force={} only_jsonl={:?} parallelism={}",
        config.claude.projects_dir.display(),
        opts.force,
        opts.only_jsonl,
        config.daemon.harvest_parallelism
    );
    let discovered = scan::discover(&config.claude.projects_dir).context("scan claude projects")?;
    let targets: Vec<_> = discovered
        .into_iter()
        .filter(|d| opts.only_jsonl.as_ref().is_none_or(|only| only == &d.jsonl_path))
        .collect();
    let total = targets.len();
    let progress = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(config.daemon.harvest_parallelism.max(1))
        .build()
        .context("build rayon thread pool for harvest")?;
    let outcomes: Vec<HarvestOne> = pool.install(|| {
        targets
            .par_iter()
            .map(|d| {
                let outcome = match harvest_one(ledger, config, &d.jsonl_path, opts.force) {
                    Ok(o) => o,
                    Err(e) => {
                        log::warn!("harvest::run: failed on {}: {e:?}", d.jsonl_path.display());
                        HarvestOne::Quarantined
                    }
                };
                let done = progress.fetch_add(1, Ordering::Relaxed) + 1;
                log::info!(
                    "harvest::run: progress {}/{} ({:?}): {}",
                    done,
                    total,
                    outcome,
                    d.jsonl_path.display()
                );
                outcome
            })
            .collect()
    });
    let mut report = HarvestReport {
        n_discovered: total,
        ..Default::default()
    };
    for o in outcomes {
        match o {
            HarvestOne::Classified => report.n_classified += 1,
            HarvestOne::SkippedUnchanged => report.n_skipped_unchanged += 1,
            HarvestOne::Quarantined => report.n_quarantined += 1,
        }
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy)]
enum HarvestOne {
    Classified,
    SkippedUnchanged,
    Quarantined,
}

fn harvest_one(ledger: &Ledger, config: &Config, path: &std::path::Path, force: bool) -> Result<HarvestOne> {
    log::debug!("harvest::harvest_one: path={} force={force}", path.display());
    let Some(session_uuid_guess) = jsonl::session_uuid_from_path(path) else {
        ledger.insert_quarantine("<unknown>", path, quarantine_reason::MALFORMED_JSONL)?;
        return Ok(HarvestOne::Quarantined);
    };
    // Idempotence early-out: if the stored sha matches, skip.
    if !force
        && let Some(stored) = ledger.get_session_sha256(&session_uuid_guess)?
        && let Ok(current) = jsonl::file_sha256(path)
        && stored == current
    {
        log::debug!("harvest::harvest_one: skip {} (sha unchanged)", session_uuid_guess);
        return Ok(HarvestOne::SkippedUnchanged);
    }

    let parsed = match jsonl::parse_session_file(path) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("harvest::harvest_one: jsonl parse failed for {}: {e}", path.display());
            ledger.insert_quarantine(
                &session_uuid_guess,
                path,
                &format!("{}: {e}", quarantine_reason::MALFORMED_JSONL),
            )?;
            return Ok(HarvestOne::Quarantined);
        }
    };
    match classify::classify(&parsed, config)? {
        ClassifyOutcome::Ok(record) => {
            ledger.upsert_session(record.as_ref()).context("upsert session")?;
            Ok(HarvestOne::Classified)
        }
        ClassifyOutcome::Quarantined { reason } => {
            ledger.insert_quarantine(&parsed.session_uuid, path, &reason)?;
            Ok(HarvestOne::Quarantined)
        }
    }
}
