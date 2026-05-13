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

    log::info!("pipeline permits initialized: general={general} heavy={heavy}");
    Ok(())
}

fn validate_cap(name: &str, cap: usize) -> Result<()> {
    if !(MIN_CAP..=MAX_CAP).contains(&cap) {
        bail!("pipeline.{name} = {cap} out of range; expected {MIN_CAP}..={MAX_CAP}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
