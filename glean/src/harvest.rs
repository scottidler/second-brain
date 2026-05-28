//! Harvest entry point: scan ~/.claude/projects, parse each JSONL,
//! run tier-1 classify, upsert into the sessions table (or quarantine).
//!
//! Idempotent: a session whose `jsonl_sha256` matches the stored row
//! is skipped unless `opts.force` is set.

use eyre::{Context, Result};

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
        "harvest::run: projects_dir={} force={} only_jsonl={:?}",
        config.claude.projects_dir.display(),
        opts.force,
        opts.only_jsonl
    );
    let discovered = scan::discover(&config.claude.projects_dir).context("scan claude projects")?;
    let mut report = HarvestReport {
        n_discovered: discovered.len(),
        ..Default::default()
    };
    for d in discovered {
        if let Some(only) = &opts.only_jsonl
            && only != &d.jsonl_path
        {
            continue;
        }
        match harvest_one(ledger, config, &d.jsonl_path, opts.force) {
            Ok(HarvestOne::Classified) => report.n_classified += 1,
            Ok(HarvestOne::SkippedUnchanged) => report.n_skipped_unchanged += 1,
            Ok(HarvestOne::Quarantined) => report.n_quarantined += 1,
            Err(e) => {
                log::warn!("harvest::run: failed on {}: {e:?}", d.jsonl_path.display());
                report.n_quarantined += 1;
            }
        }
    }
    Ok(report)
}

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
