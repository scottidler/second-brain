//! Fabric pattern integration for LLM-powered features.
//!
//! Thin wrapper around `vault::fabric::run_pattern` so every fabric
//! invocation in the workspace shares one pattern resolver
//! (`vault::fabric::resolve_pattern` reading from
//! `vault::paths::patterns_dir()`) and one set of knobs (binary, model,
//! max_content_chars) from the cortex FabricConfig.
//!
//! Per-feature timeouts stay tunable (autotag 120s, classify 30s,
//! intel 120s); binary/model/max-chars are global.

use eyre::Result;

use crate::config::FabricConfig;

/// Run a Fabric pattern against input text, using the cortex
/// `FabricConfig` for binary/model/max-chars and the caller-supplied
/// `timeout_secs` for the per-feature timeout.
///
/// Routes through `vault::fabric::run_pattern`, which resolves the
/// pattern name against `vault::paths::patterns_dir()`.
pub fn run_pattern(fabric: &FabricConfig, pattern: &str, input: &str, timeout_secs: u64) -> Result<String> {
    log::debug!(
        "cortex::fabric::run_pattern: pattern={pattern} binary={} model={} max_chars={} timeout_secs={timeout_secs} input_len={}",
        fabric.binary,
        fabric.model,
        fabric.max_content_chars,
        input.len()
    );
    vault::fabric::run_pattern(
        pattern,
        input,
        &fabric.binary,
        &fabric.api_key,
        &fabric.model,
        fabric.max_content_chars,
        timeout_secs,
    )
}

/// Check if fabric is available on the system. Uses `which` rather than
/// invoking the binary (some fabric subcommands hang on first call).
pub fn is_available(binary: &str) -> bool {
    vault::fabric::is_available(binary)
}

// `truncate_input` lives in `crate::llm` (single source of truth); re-exported
// here so the many `crate::fabric::truncate_input` call sites stay valid.
pub use crate::llm::truncate_input;

#[cfg(test)]
mod tests;
