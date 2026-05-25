//! Process-startup helpers for cortex.
//!
//! `validate_canonical_assets` is the precondition every cortex consumer
//! entry point (daemon main loop, sweep/intel/classify/migrate one-shots)
//! must call before any work that touches the canonical-tag vocabulary
//! or tag mapping. Cortex does not consume the patterns directory
//! directly (patterns are reached via `vault::fabric::run_pattern`,
//! which has its own missing-pattern error path), so this helper omits
//! that check.

use eyre::{Result, bail};

/// Refuse to proceed unless canonical-tags + tag-mapping are present and
/// parse. Same shape as `borg::startup::validate_canonical_assets` minus
/// the patterns-dir check.
///
/// Every consumer entry point (sweep::run, sweep::migrate,
/// sweep::scan_proposals, intel::run, classify::run,
/// daemon::start_watching, migrate::apply) calls this as its first
/// statement so a fresh install hits one actionable error message, not
/// six opaque "failed to load canonical tags" wrapper errors.
pub fn validate_canonical_assets() -> Result<()> {
    let canonical = vault::paths::canonical_tags();
    if !canonical.exists() {
        bail!(
            "missing canonical-tags vocabulary at {}\n\
             run `sb bootstrap` to provision (or `sb bootstrap --force` to refresh from the binary's embedded copy)",
            canonical.display()
        );
    }
    vault::canonical::CanonicalTagsFile::load(&canonical).map_err(|e| {
        eyre::eyre!(
            "canonical-tags vocabulary at {} failed to parse: {e}\n\
         run `sb bootstrap --force` to restore from the binary's embedded copy",
            canonical.display()
        )
    })?;

    let mapping = vault::paths::tag_mapping();
    if !mapping.exists() {
        bail!(
            "missing tag-mapping at {}\n\
             run `sb bootstrap` to provision (or `sb bootstrap --force` to refresh from the binary's embedded copy)",
            mapping.display()
        );
    }
    vault::canonical::load_tag_mapping(&mapping).map_err(|e| {
        eyre::eyre!(
            "tag-mapping at {} failed to parse: {e}\n\
         run `sb bootstrap --force` to restore from the binary's embedded copy",
            mapping.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests;
