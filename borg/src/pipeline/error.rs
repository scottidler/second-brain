//! `PipelineError`: typed wrapper that pairs an `eyre::Report` with the
//! `FailureStage` it should be classified under.
//!
//! `process_url_inner` and every fallible inner stage return
//! `Result<T, PipelineError>` instead of `eyre::Result<T>`. The outer
//! `process_url` catch-all receives a typed error, reads `.stage`, and writes
//! the receipts row with the right `failure_stage`. The compiler ensures
//! every error site is classified at the point the error becomes terminal;
//! there is no string-matching fallback.
//!
//! This is the only mechanism for receipts failure-stage classification in
//! the pipeline path.

use vault::receipts::FailureStage;

#[derive(Debug)]
pub struct PipelineError {
    pub stage: FailureStage,
    pub source: eyre::Report,
}

impl PipelineError {
    pub fn new(stage: FailureStage, source: impl Into<eyre::Report>) -> Self {
        Self {
            stage,
            source: source.into(),
        }
    }

    /// Convenience constructor for the most common shape: an existing
    /// `eyre::Report` augmented with a stage classification.
    pub fn from_report(stage: FailureStage, source: eyre::Report) -> Self {
        Self { stage, source }
    }
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.stage, self.source)
    }
}

// Note: we deliberately do NOT implement `std::error::Error` for
// `PipelineError`. Doing so would conflict with eyre's blanket
// `impl<E: Error + ...> From<E> for Report`, and the whole point of this
// wrapper is that conversion to `eyre::Report` goes through the impl below
// (which preserves the stage in the error message).
impl From<PipelineError> for eyre::Report {
    fn from(e: PipelineError) -> Self {
        e.source.wrap_err(format!("pipeline stage {} failed", e.stage))
    }
}

#[cfg(test)]
mod tests;
