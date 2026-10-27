//! ONNX Runtime plumbing shared by every ONNX model in the process.
//!
//! `ort` 2.0.0-rc.15 allows one ORT environment per process, so the embedder
//! (`vault::embedding::onnx`) and the cross-encoder build their sessions
//! through [`build_session`], which owns that single environment.
//!
//! Two invariants live here so no caller can skip them:
//!
//! - **CPU gate.** [`build_session`] refuses a CPU that is not
//!   [`CpuFeatures::onnx_capable`] before any `ort` call. ORT's static archive
//!   faults on its first model parse on a CPU without AVX2 (measured on `talos`).
//! - **Log volume.** ORT logs hundreds of INFO lines per session. The
//!   environment forwards ORT's own log stream to the `log` crate under the
//!   `ort` target, warnings and errors only, and raises ORT's threshold to
//!   warning once built so later INFO and VERBOSE lines are not even produced.

use eyre::{Result, eyre};
use ort::environment::Environment;
use ort::logging::LogLevel;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::cpu::CpuFeatures;

/// The `log` target ORT's own messages arrive under.
const ORT_LOG_TARGET: &str = "ort";

/// Refuse a CPU ONNX Runtime cannot run on, naming what it lacks.
pub fn require_onnx_capable(cpu: &CpuFeatures) -> Result<()> {
    let missing = cpu.missing_for_onnx();
    if missing.is_empty() {
        return Ok(());
    }
    Err(eyre!(
        "ONNX Runtime needs AVX2 and FMA; this CPU lacks {}",
        missing.join(" and ")
    ))
}

/// Build a session for `model` with `intra_threads` intra-op threads, one
/// inter-op thread and full graph optimization.
///
/// `cpu` is checked first: an incapable CPU returns `Err` before the
/// environment or the session is touched.
pub fn build_session(model: &Path, intra_threads: usize, cpu: &CpuFeatures) -> Result<Session> {
    log::debug!(
        "onnx::build_session: model={} intra_threads={intra_threads}",
        model.display()
    );
    require_onnx_capable(cpu)?;
    if intra_threads == 0 {
        eyre::bail!("onnx::build_session: intra_threads must be >= 1");
    }
    let env = environment()?;
    let session = Session::builder(env)
        .map_err(|e| eyre!("ort session builder: {e}"))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| eyre!("ort optimization level: {e}"))?
        .with_intra_threads(intra_threads)
        .map_err(|e| eyre!("ort intra threads: {e}"))?
        .with_inter_threads(1)
        .map_err(|e| eyre!("ort inter threads: {e}"))?
        .commit_from_file(model)
        .map_err(|e| eyre!("ort load {}: {e}", model.display()))?;
    log::debug!("onnx::build_session: committed {}", model.display());
    Ok(session)
}

/// The process's one ORT environment, built on first use.
///
/// A failed build is cached too: ORT refuses a second environment per process,
/// so a retry could not succeed, and every later caller gets the same error.
fn environment() -> Result<&'static Environment> {
    static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        let env = ort::init()
            .with_name("sb")
            .with_logger(Arc::new(forward_ort_log))
            .build()
            .map_err(|e| format!("ort environment: {e}"))?;
        env.set_log_level(LogLevel::Warning);
        log::info!("onnx::environment: {}", ort::info());
        Ok(env)
    })
    .as_ref()
    .map_err(|e| eyre!("{e}"))
}

/// ORT's log callback. The environment is created at VERBOSE and only lowered
/// to warning after `build` returns, and the build itself logs (hardware
/// device discovery at INFO), so the threshold is enforced here as well.
fn forward_ort_log(level: LogLevel, category: &str, id: &str, location: &str, message: &str) {
    if let Some(level) = forwarded_level(level) {
        log::log!(target: ORT_LOG_TARGET, level, "[{category}/{id}] {message} ({location})");
    }
}

/// The `log` level an ORT message is forwarded at; `None` for INFO and
/// VERBOSE, which ORT emits by the hundred per session.
fn forwarded_level(level: LogLevel) -> Option<log::Level> {
    match level {
        LogLevel::Verbose | LogLevel::Info => None,
        LogLevel::Warning => Some(log::Level::Warn),
        LogLevel::Error | LogLevel::Fatal => Some(log::Level::Error),
    }
}

#[cfg(test)]
mod tests;
