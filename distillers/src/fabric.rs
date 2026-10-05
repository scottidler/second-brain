//! Fabric integration port.
//!
//! `FabricCaller` lets distillers be generic over how they reach Fabric:
//! production uses `FabricShell` (delegates to `vault::fabric::run_pattern`);
//! tests use `FakeFabric` (returns canned YAML keyed by pattern id).

use async_trait::async_trait;
use eyre::{Context, Result};

/// All the knobs a single Fabric invocation needs. Kept narrow so tests can
/// match on `pattern` alone.
#[derive(Debug, Clone)]
pub struct FabricRequest {
    pub pattern: String,
    pub input: String,
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

/// Port over a Fabric invocation. Generic over the concrete caller so each
/// distiller can be tested with a deterministic fake.
#[async_trait]
pub trait FabricCaller: Send + Sync {
    async fn call(&self, request: FabricRequest) -> Result<String>;
}

#[async_trait]
impl<F: FabricCaller + ?Sized> FabricCaller for std::sync::Arc<F> {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        (**self).call(request).await
    }
}

/// Production caller. Shells out to the `fabric` binary on the tokio blocking
/// pool because the underlying helper is sync.
#[derive(Debug, Clone)]
pub struct FabricShell {
    pub binary: String,
    /// Output-token ceiling handed to every call as `--maxTokens=<n>`. 0 means
    /// "leave it to fabric", which is the safe value on an unpatched fabric
    /// that has no such flag. Lives on the CALLER, not on `FabricRequest`: it
    /// is a transport setting uniform across every pattern, so putting it here
    /// keeps the ~10 `FabricRequest` construction sites untouched.
    pub max_tokens: usize,
    /// NAME of the env var (or file path) holding the Anthropic credential the
    /// fabric child needs under the literal name `ANTHROPIC_API_KEY`. Threaded
    /// from the caller's `FabricConfig.api-key` (which borg/cortex mirror from
    /// `llm.api-key`); empty leaves the child's `ANTHROPIC_API_KEY` untouched.
    pub api_key: String,
}

impl FabricShell {
    /// Construct with no output-token ceiling (fabric's own default).
    pub fn new(binary: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            api_key: api_key.into(),
            max_tokens: 0,
        }
    }

    /// Construct with an explicit output-token ceiling. See
    /// [`FabricShell::max_tokens`].
    pub fn with_max_tokens(binary: impl Into<String>, api_key: impl Into<String>, max_tokens: usize) -> Self {
        Self {
            binary: binary.into(),
            api_key: api_key.into(),
            max_tokens,
        }
    }
}

#[async_trait]
impl FabricCaller for FabricShell {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        let binary = self.binary.clone();
        let api_key = self.api_key.clone();
        let max_tokens = self.max_tokens;
        let FabricRequest {
            pattern,
            input,
            model,
            max_chars,
            timeout_secs,
        } = request;
        log::debug!(
            "FabricShell::call: pattern={} model={} max_chars={} timeout_secs={} max_tokens={} input_len={}",
            pattern,
            model,
            max_chars,
            timeout_secs,
            max_tokens,
            input.len()
        );
        tokio::task::spawn_blocking(move || {
            vault::fabric::run_pattern_with_max_tokens(
                &pattern,
                &input,
                &binary,
                &api_key,
                &model,
                max_chars,
                timeout_secs,
                max_tokens,
            )
        })
        .await
        .context("fabric task panicked")?
    }
}

#[cfg(any(test, feature = "test-util"))]
mod fake;
#[cfg(any(test, feature = "test-util"))]
pub use fake::FakeFabric;
