//! Process-startup helpers.
//!
//! `init_permits` sizes the two `pipeline::permits` pools from config. It is
//! called once per borg process (daemon and CLI alike) before any code path
//! that can reach `pipeline::process_content`. Calling `permits::PermitPool::acquire`
//! before `init_permits` panics by design; this helper is the only sanctioned
//! initialization site.

use eyre::{Result, bail};

use crate::config::Config;
use crate::pipeline::permits;

const MIN_CAP: usize = 1;
const MAX_CAP: usize = 64;

/// Initialize the general and heavy permit pools from `cfg.pipeline`. Validates
/// each cap is in `[1, 64]` so a misconfigured `0` does not deadlock all
/// subsequent acquires and an accidental `999999` does not silently disable
/// the cap.
pub fn init_permits(cfg: &Config) -> Result<()> {
    let general = cfg.pipeline.max_concurrent_traces;
    let heavy = cfg.pipeline.max_concurrent_heavy_traces;
    log::debug!("init_permits: general={general} heavy={heavy} (range: {MIN_CAP}..={MAX_CAP})");

    validate_cap("max-concurrent-traces", general)?;
    validate_cap("max-concurrent-heavy-traces", heavy)?;

    permits::GENERAL_PERMITS.init(general);
    permits::HEAVY_PERMITS.init(heavy);

    // Size the process-wide vision permit pool from the content-filter config.
    // This is the single sanctioned init site (same place the pipeline permit
    // pools are sized): it runs once per borg process - daemon and CLI alike,
    // via `sb borg`'s `BorgCli::run` - before any path that can reach
    // `pipeline::process_content`, and therefore before any vision call from
    // `try_extract_slides` or the image-ingest `vision_extract` path. The pool's
    // own `init_vision_permits` clamps `cap.max(1)`, so an over-eager `0` cannot
    // wedge the gate; until this runs the pool is ungated by design.
    let vision_cap = cfg.youtube.slides.content_filter.max_vision_concurrency;
    log::debug!("init_permits: vision={vision_cap}");
    crate::ocr::init_vision_permits(vision_cap);

    log::debug!("pipeline permits initialized: general={general} heavy={heavy} vision={vision_cap}");
    Ok(())
}

fn validate_cap(name: &str, cap: usize) -> Result<()> {
    if !(MIN_CAP..=MAX_CAP).contains(&cap) {
        bail!("pipeline.{name} = {cap} out of range; expected {MIN_CAP}..={MAX_CAP}");
    }
    Ok(())
}

/// Emit one DEBUG line summarizing the configured-vs-resolved ffmpeg thread
/// caps, so an operator running with `--log-level debug` can confirm from
/// the journal how `nproc/N` expressions resolved against this host's `nproc`.
/// At default (INFO) level this stays silent, so one-shot inspection
/// commands don't pay the noise cost.
pub fn log_ffmpeg_thread_caps(cfg: &Config) {
    let nproc = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let threads_resolved = cfg.youtube.ffmpeg_threads.resolve();
    let filter_resolved = cfg.youtube.ffmpeg_filter_threads.resolve();
    let threads_sym = cfg.youtube.ffmpeg_threads.symbolic();
    let filter_sym = cfg.youtube.ffmpeg_filter_threads.symbolic();
    log::debug!(
        "ffmpeg thread caps: threads={threads_resolved} filter_threads={filter_resolved} \
         (nproc={nproc}, ffmpeg-threads={threads_sym}, ffmpeg-filter-threads={filter_sym})"
    );
}

/// Precondition: every consumer of canonical-tag filtering or the fabric
/// patterns directory must call this before any work that can touch the
/// vocabulary. Verifies presence AND parseability so a malformed file
/// fails as loudly as a missing one.
///
/// Bails with an actionable error message naming `sb bootstrap`
/// (write-if-missing) or `sb bootstrap --force` (refresh from binary).
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

    let patterns = vault::paths::patterns_dir();
    if !patterns.is_dir() {
        bail!(
            "missing fabric patterns directory at {}\n\
             run `sb bootstrap` to provision (or `sb bootstrap --force` to refresh from the binary's embedded copy)",
            patterns.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests;
