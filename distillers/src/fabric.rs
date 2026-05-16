//! Fabric integration port.
//!
//! `FabricCaller` lets distillers be generic over how they reach Fabric:
//! production uses `FabricShell` (delegates to `vault::fabric::run_pattern`);
//! tests use `FakeFabric` (returns canned YAML keyed by pattern id).

use async_trait::async_trait;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::sync::Mutex;

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
}

impl FabricShell {
    pub fn new(binary: impl Into<String>) -> Self {
        Self { binary: binary.into() }
    }
}

#[async_trait]
impl FabricCaller for FabricShell {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        let binary = self.binary.clone();
        let FabricRequest {
            pattern,
            input,
            model,
            max_chars,
            timeout_secs,
        } = request;
        log::debug!(
            "FabricShell::call: pattern={} model={} max_chars={} timeout_secs={} input_len={}",
            pattern,
            model,
            max_chars,
            timeout_secs,
            input.len()
        );
        tokio::task::spawn_blocking(move || {
            vault::fabric::run_pattern(&pattern, &input, &binary, &model, max_chars, timeout_secs)
        })
        .await
        .context("fabric task panicked")?
    }
}

/// Test caller. Returns canned YAML stdout per pattern id; defaults to a
/// minimal valid Distilled YAML so unrelated tests don't have to seed it.
#[derive(Debug, Default)]
pub struct FakeFabric {
    inner: Mutex<FakeInner>,
}

#[derive(Debug, Default)]
struct FakeInner {
    responses: HashMap<String, FakeResponse>,
    calls: Vec<FabricRequest>,
}

#[derive(Debug, Clone)]
enum FakeResponse {
    Ok(String),
    Err(String),
}

impl FakeFabric {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a canned YAML body for the next call matching `pattern`.
    pub fn set_response(&self, pattern: impl Into<String>, yaml_body: impl Into<String>) {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        inner
            .responses
            .insert(pattern.into(), FakeResponse::Ok(yaml_body.into()));
    }

    /// Queue an error for the next call matching `pattern`. The body becomes
    /// the eyre message so distillers can branch on the error string.
    pub fn set_error(&self, pattern: impl Into<String>, message: impl Into<String>) {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        inner
            .responses
            .insert(pattern.into(), FakeResponse::Err(message.into()));
    }

    /// Inspect what callers asked for.
    pub fn calls(&self) -> Vec<FabricRequest> {
        let inner = self.inner.lock().expect("FakeFabric poisoned");
        inner.calls.clone()
    }
}

#[async_trait]
impl FabricCaller for FakeFabric {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        inner.calls.push(request.clone());
        match inner.responses.get(&request.pattern) {
            Some(FakeResponse::Ok(body)) => Ok(body.clone()),
            Some(FakeResponse::Err(msg)) => Err(eyre::eyre!(msg.clone())),
            None => Err(eyre::eyre!(
                "FakeFabric: no canned response for pattern {}",
                request.pattern
            )),
        }
    }
}
