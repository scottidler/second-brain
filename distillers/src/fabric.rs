//! Fabric integration port.
//!
//! `FabricCaller` lets distillers be generic over how they reach Fabric:
//! production uses `FabricShell` (delegates to `vault::fabric::run_pattern`);
//! tests use `FakeFabric` (returns canned YAML keyed by pattern id).

use async_trait::async_trait;
use eyre::{Context, Result};
use std::collections::{HashMap, VecDeque};
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
    /// NAME of the env var (or file path) holding the Anthropic credential the
    /// fabric child needs under the literal name `ANTHROPIC_API_KEY`. Threaded
    /// from the caller's `FabricConfig.api-key` (which borg/cortex mirror from
    /// `llm.api-key`); empty leaves the child's `ANTHROPIC_API_KEY` untouched.
    pub api_key: String,
}

impl FabricShell {
    pub fn new(binary: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl FabricCaller for FabricShell {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        let binary = self.binary.clone();
        let api_key = self.api_key.clone();
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
            vault::fabric::run_pattern(&pattern, &input, &binary, &api_key, &model, max_chars, timeout_secs)
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
    /// Per-pattern FIFO queue of outcomes consumed before the steady
    /// `responses` entry. Lets a test make some calls to the same pattern fail
    /// and others succeed (e.g. one chunk fails, the rest distill cleanly).
    sequences: HashMap<String, VecDeque<FakeResponse>>,
    calls: Vec<FabricRequest>,
}

#[derive(Debug, Clone)]
enum FakeResponse {
    Ok(String),
    Err(String),
    /// Inject a real `vault::fabric::FabricError::Timeout` so distiller tests
    /// exercise the typed-timeout detection path (not a string match).
    Timeout,
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

    /// Queue a typed `FabricError::Timeout` for the next call matching
    /// `pattern`, so a distiller's timeout-fallback path is driven by the same
    /// typed error production emits (not a fragile message substring).
    pub fn set_timeout(&self, pattern: impl Into<String>) {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        inner.responses.insert(pattern.into(), FakeResponse::Timeout);
    }

    /// Queue a FIFO sequence of per-call outcomes for `pattern`, consumed in
    /// order before the steady `set_response` / `set_error` / `set_timeout`
    /// entry. `Ok(body)` yields that YAML body; `Err(msg)` yields an eyre
    /// error. Once the queue drains, calls fall through to the steady entry
    /// (or the no-canned-response error). The `call` impl holds the lock for
    /// the whole call, so concurrent (`buffer_unordered`) chunk calls drain the
    /// queue deterministically regardless of completion order.
    pub fn set_response_sequence(
        &self,
        pattern: impl Into<String>,
        outcomes: Vec<std::result::Result<String, String>>,
    ) {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        let queue = outcomes
            .into_iter()
            .map(|o| match o {
                Ok(body) => FakeResponse::Ok(body),
                Err(msg) => FakeResponse::Err(msg),
            })
            .collect();
        inner.sequences.insert(pattern.into(), queue);
    }

    /// Inspect what callers asked for.
    pub fn calls(&self) -> Vec<FabricRequest> {
        let inner = self.inner.lock().expect("FakeFabric poisoned");
        inner.calls.clone()
    }
}

/// Resolve a single canned outcome into the `call` return value. Free fn so
/// both the sequence path and the steady-response path share it.
fn fake_response_to_result(response: &FakeResponse, request: &FabricRequest) -> Result<String> {
    match response {
        FakeResponse::Ok(body) => Ok(body.clone()),
        FakeResponse::Err(msg) => Err(eyre::eyre!(msg.clone())),
        FakeResponse::Timeout => Err(vault::fabric::FabricError::Timeout {
            pattern: request.pattern.clone(),
            timeout_secs: request.timeout_secs,
        }
        .into()),
    }
}

#[async_trait]
impl FabricCaller for FakeFabric {
    async fn call(&self, request: FabricRequest) -> Result<String> {
        let mut inner = self.inner.lock().expect("FakeFabric poisoned");
        inner.calls.push(request.clone());
        // A queued sequence (while non-empty) takes precedence so a test can
        // fail some calls to a pattern and succeed others.
        if let Some(queue) = inner.sequences.get_mut(&request.pattern)
            && let Some(response) = queue.pop_front()
        {
            return fake_response_to_result(&response, &request);
        }
        match inner.responses.get(&request.pattern) {
            Some(response) => fake_response_to_result(response, &request),
            None => Err(eyre::eyre!(
                "FakeFabric: no canned response for pattern {}",
                request.pattern
            )),
        }
    }
}
