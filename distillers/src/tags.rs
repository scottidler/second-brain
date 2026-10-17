//! Shared closed-vocabulary tag classifier.
//!
//! One seam, three implementations, called by borg at ingest (`finalize_tags`)
//! and by cortex at classify / `--retag`. It lives in `distillers` because that
//! is the crate both callers already depend on and because `vault` stays
//! LLM-free (`oracle/AGENTS.md`).
//!
//! The trait is **synchronous**: cortex's `classify_note` is a sync fn and
//! deliberately has no tokio runtime (`cortex/src/llm.rs` uses blocking `ureq`
//! for the same reason). Borg's `finalize_tags` is async and wraps the call at
//! its own call site. That is also why `FabricClosedVocab` takes the sync
//! `FabricRunner` port below rather than the crate's async `FabricCaller`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eyre::{Context, Result, eyre};
use serde::{Deserialize, Serialize};
use vault::canonical::{self, CanonicalSet, TagMapping};

#[cfg(test)]
mod tests;

/// TypeSafe's API root. Production always talks to this host; only a
/// `test-util` build can swap it, through `TagsClassifierConfig::base_url`.
const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// The one endpoint the jev classifier calls: one `state` against N questions.
const SYSTEMONE_PATH: &str = "/v1/systemone";

/// What a 429's retry must leave for the retried request itself: the retry
/// runs only if `delay + RETRY_REQUEST_ALLOWANCE` still fits the deadline.
const RETRY_REQUEST_ALLOWANCE: Duration = Duration::from_secs(1);

/// The TypeSafe key's env var when `api-key` is not set.
const DEFAULT_API_KEY_ENV: &str = "ESCOTE_TYPESAFE_API_KEY";

/// `jev-latest` rather than a pinned id: `/v1/models` lists only
/// `jev-latest` and `jev-preview`, so a pin can 400 once it is retired.
const DEFAULT_MODEL: &str = "jev-latest";

/// Where a candidate tag came from. Provenance decides which implementations
/// are allowed to consume it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// Publisher hashtags, yt-dlp tags, and the note's `author-tags` key.
    /// Deterministic and human-supplied, so every impl unions these at tier 0.
    Author,
    /// Distiller / LLM output from this ingest. Ignored by the scoring impls:
    /// feeding yesterday's LLM output back in carries its drift forward.
    Model,
    /// The note's existing canonical `tags` (cortex call sites only). Consumed
    /// by `Deterministic`, which is today's Tier 1; the merge policy, not the
    /// classifier, is what preserves these on reingest.
    Preserved,
}

#[derive(Debug, Clone)]
pub struct TagCandidate {
    pub text: String,
    pub source: CandidateSource,
}

impl TagCandidate {
    pub fn new(text: impl Into<String>, source: CandidateSource) -> Self {
        Self {
            text: text.into(),
            source,
        }
    }
}

/// What a classifier scores. `text` is the note summary when one exists, else
/// a `CLASSIFIER_TEXT_CHARS` body excerpt. Never a whole transcript.
#[derive(Debug, Clone, Copy)]
pub struct TagInput<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub candidates: &'a [TagCandidate],
}

/// How much body text the scoring classifiers read when a note has no
/// summary (design doc, API Design).
///
/// This is a data-boundary number, not a tuning knob: `text` is shipped
/// verbatim to TypeSafe by the jev classifier, so an untruncated body means a whole
/// transcript of captured audio leaves the machine. Both callers clip
/// through `body_excerpt` (borg: `TagSources::from_body`; cortex:
/// `classify::classifier_text`).
pub const CLASSIFIER_TEXT_CHARS: usize = 900;

/// The head of a note body, clipped to `CLASSIFIER_TEXT_CHARS`. Counts
/// characters rather than bytes so a multi-byte boundary cannot panic.
pub fn body_excerpt(body: &str) -> String {
    body.chars().take(CLASSIFIER_TEXT_CHARS).collect()
}

/// Drives inbox promotion and `cortex-confidence`. Promotion happens on `High`
/// or `Medium`; `Low` holds the note for the next tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMethod {
    Deterministic,
    FabricClosed,
    Jev,
}

impl TagMethod {
    /// Written to `cortex-classified-by`.
    pub fn as_str(&self) -> &'static str {
        match self {
            TagMethod::Deterministic => "deterministic",
            TagMethod::FabricClosed => "fabric-closed",
            TagMethod::Jev => "jev",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TagOutput {
    pub tags: Vec<String>,
    /// Present only for the scoring impls; `Deterministic` and `FabricClosed`
    /// return `None` rather than inventing a number.
    pub scores: Option<Vec<(String, f32)>>,
    pub confidence: Confidence,
    pub method: TagMethod,
}

impl TagOutput {
    fn empty(method: TagMethod) -> Self {
        Self {
            tags: Vec::new(),
            scores: None,
            confidence: Confidence::Low,
            method,
        }
    }
}

pub trait TagClassifier: Send + Sync {
    fn classify(&self, input: &TagInput) -> Result<TagOutput>;

    fn method(&self) -> TagMethod;
}

// ---------------------------------------------------------------- config

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClassifierKind {
    #[default]
    Deterministic,
    FabricClosed,
    Jev,
}

/// The `tags.classifier` block, identical in `borg.yml` and `cortex.yml`.
///
/// Unknown keys are an error, so a stale key from an older classifier fails
/// the load naming the field instead of being silently ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct TagsClassifierConfig {
    pub classifier: ClassifierKind,
    pub threshold: f32,
    /// Runs when the selected impl errors. Borg marks the receipt degraded;
    /// cortex holds the note only if the fallback also returns `Low`.
    pub fallback: ClassifierKind,
    /// One deadline per `classify`: request, any 429 sleep, the retry, and
    /// the body read all fit inside it.
    pub timeout_secs: u64,
    /// NAME of the env var (or a secret file path, per
    /// `vault::config::resolve_secret`) holding the TypeSafe key, never the
    /// key itself. A name that resolves empty fails every jev classify before
    /// any request, so the fallback runs.
    pub api_key: String,
    /// The jev model id sent on every request.
    pub model: String,
    /// Test seam: the API root a stub serves. Not in yml and absent from a
    /// release build, so production always calls `DEFAULT_BASE_URL`.
    #[cfg(any(test, feature = "test-util"))]
    #[serde(skip)]
    pub base_url: Option<String>,
}

impl Default for TagsClassifierConfig {
    fn default() -> Self {
        Self {
            classifier: ClassifierKind::Deterministic,
            threshold: 0.9,
            fallback: ClassifierKind::Deterministic,
            timeout_secs: 30,
            api_key: DEFAULT_API_KEY_ENV.to_string(),
            model: DEFAULT_MODEL.to_string(),
            #[cfg(any(test, feature = "test-util"))]
            base_url: None,
        }
    }
}

/// Sync port over a Fabric pattern invocation.
///
/// The crate's `FabricCaller` is `#[async_trait]`, and `TagClassifier` is sync
/// by design (see the module docs), so `FabricClosedVocab` takes this instead.
/// The production impl calls the same underlying `vault::fabric` helper that
/// `FabricShell` wraps in `spawn_blocking`.
pub trait FabricRunner: Send + Sync {
    fn run(&self, pattern: &str, input: &str) -> Result<String>;
}

/// Production runner: shells out to the `fabric` binary, synchronously.
#[derive(Debug, Clone)]
pub struct ShellFabricRunner {
    pub binary: String,
    pub api_key: String,
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl FabricRunner for ShellFabricRunner {
    fn run(&self, pattern: &str, input: &str) -> Result<String> {
        log::debug!(
            "ShellFabricRunner::run: pattern={pattern} binary={} input_len={}",
            self.binary,
            input.len()
        );
        vault::fabric::run_pattern(
            pattern,
            input,
            &self.binary,
            &self.api_key,
            &self.model,
            self.max_chars,
            self.timeout_secs,
        )
    }
}

pub fn build(
    cfg: &TagsClassifierConfig,
    canon: &CanonicalSet,
    mapping: &TagMapping,
    fabric: Option<Arc<dyn FabricRunner>>,
) -> Box<dyn TagClassifier> {
    build_kind(cfg.classifier, cfg, canon, mapping, fabric)
}

/// Build the configured `fallback` impl, for the error path at both call sites.
pub fn build_fallback(
    cfg: &TagsClassifierConfig,
    canon: &CanonicalSet,
    mapping: &TagMapping,
    fabric: Option<Arc<dyn FabricRunner>>,
) -> Box<dyn TagClassifier> {
    build_kind(cfg.fallback, cfg, canon, mapping, fabric)
}

fn build_kind(
    kind: ClassifierKind,
    cfg: &TagsClassifierConfig,
    canon: &CanonicalSet,
    mapping: &TagMapping,
    fabric: Option<Arc<dyn FabricRunner>>,
) -> Box<dyn TagClassifier> {
    log::debug!("tags::build_kind: kind={kind:?} threshold={}", cfg.threshold);
    match kind {
        ClassifierKind::Deterministic => Box::new(Deterministic::new(canon.clone(), mapping.clone())),
        ClassifierKind::FabricClosed => match fabric {
            Some(runner) => Box::new(FabricClosedVocab::new(canon.clone(), mapping.clone(), runner)),
            // No runner means no way to call Fabric; degrade to the impl that
            // needs nothing rather than hand back a classifier that always errors.
            None => Box::new(Deterministic::new(canon.clone(), mapping.clone())),
        },
        ClassifierKind::Jev => Box::new(Jev::new(canon.clone(), mapping.clone(), cfg)),
    }
}

// ------------------------------------------------------- shared helpers

/// Which matcher tier produced a tag. Lower is stronger, and the ordering is
/// what `filter_and_cap` already applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Mapping = 0,
    Exact = 1,
    Segment = 2,
}

/// Map one raw candidate through the vocabulary, reporting the tier that hit.
fn match_with_tier(raw: &str, canon: &CanonicalSet, mapping: &TagMapping) -> Vec<(String, Tier)> {
    let tier = if mapping.contains_key(raw) {
        Tier::Mapping
    } else if canon.all.contains(raw) {
        Tier::Exact
    } else {
        Tier::Segment
    };
    canonical::match_to_canonical(raw, canon, mapping)
        .into_iter()
        .map(|t| (t, tier))
        .collect()
}

/// Author candidates that map are unioned at tier 0 by every implementation:
/// they are publisher-provided and deterministic.
fn author_tags(input: &TagInput, canon: &CanonicalSet, mapping: &TagMapping) -> Vec<String> {
    let mut out = Vec::new();
    for c in input.candidates.iter().filter(|c| c.source == CandidateSource::Author) {
        for (tag, _) in match_with_tier(&c.text, canon, mapping) {
            if !out.contains(&tag) {
                out.push(tag);
            }
        }
    }
    out
}

/// Sort by score descending then name, dedupe, and cap. The single place the
/// output ordering invariant is enforced.
fn rank_and_cap(mut scored: Vec<(String, f32)>, max_per_note: usize) -> Vec<(String, f32)> {
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut seen = HashSet::new();
    scored.retain(|(tag, _)| seen.insert(tag.clone()));
    scored.truncate(max_per_note);
    scored
}

/// Apply the threshold, then rank and cap. Selection and promotion share the
/// threshold on purpose, so a promoted note carries the tags that promoted it.
pub fn select_above_threshold(scores: &[(String, f32)], threshold: f32, max_per_note: usize) -> Vec<(String, f32)> {
    let kept: Vec<(String, f32)> = scores
        .iter()
        .filter(|(_, s)| *s >= threshold)
        .map(|(t, s)| (t.clone(), *s))
        .collect();
    rank_and_cap(kept, max_per_note)
}

/// `--retag` is replace, not union, with two guards: a tag in the protect list
/// (`no-classifier-tags`) is never dropped, and an empty or `Low` result never
/// overwrites what is already there.
///
/// The protect list exists because the classifier recovers 0 of 17 migrated
/// `work`/`life`/`homelab`/`diy`/`writing` values from note text.
///
/// The result is capped through `canonical::cap_protecting`, the same helper
/// `filter_and_cap` uses: prepending protected tags to an already-capped
/// fresh set used to write over `max-per-note`, and the next daemon sweep
/// then re-capped the note without a protect list and dropped the protected
/// tag (implementation audit r1, M1).
pub fn apply_retag(previous: &[String], fresh: &TagOutput, canon: &CanonicalSet) -> Vec<String> {
    if fresh.tags.is_empty() || fresh.confidence == Confidence::Low {
        log::debug!(
            "tags::apply_retag: keeping {} existing tags, fresh result was {:?}/{} tags",
            previous.len(),
            fresh.confidence,
            fresh.tags.len()
        );
        return previous.to_vec();
    }
    let mut out: Vec<String> = previous
        .iter()
        .filter(|t| canon.no_classifier.contains(*t))
        .cloned()
        .collect();
    for tag in &fresh.tags {
        if !out.contains(tag) {
            out.push(tag.clone());
        }
    }
    canonical::cap_protecting(out, canon.max_per_note, &canon.no_classifier)
}

// -------------------------------------------------------- deterministic

/// Pure canonical mapping, no network and no LLM. Consumes every candidate
/// source, which at cortex call sites is exactly today's Tier 1.
pub struct Deterministic {
    canon: CanonicalSet,
    mapping: TagMapping,
}

impl Deterministic {
    pub fn new(canon: CanonicalSet, mapping: TagMapping) -> Self {
        Self { canon, mapping }
    }
}

impl TagClassifier for Deterministic {
    fn classify(&self, input: &TagInput) -> Result<TagOutput> {
        let mut hits: Vec<(String, Tier)> = Vec::new();
        for c in input.candidates {
            for (tag, tier) in match_with_tier(&c.text, &self.canon, &self.mapping) {
                hits.push((tag, tier));
            }
        }
        if hits.is_empty() {
            return Ok(TagOutput::empty(TagMethod::Deterministic));
        }

        // Tier ascending then name: the same ordering `filter_and_cap` applies.
        // Scores would be invented here, so the ranking key is the tier and
        // `scores` stays `None`.
        hits.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut seen = HashSet::new();
        hits.retain(|(tag, _)| seen.insert(tag.clone()));
        let strongest = hits.iter().map(|(_, t)| *t).min().unwrap_or(Tier::Segment);
        hits.truncate(self.canon.max_per_note);

        let confidence = if strongest <= Tier::Exact { Confidence::High } else { Confidence::Medium };
        Ok(TagOutput {
            tags: hits.into_iter().map(|(t, _)| t).collect(),
            scores: None,
            confidence,
            method: TagMethod::Deterministic,
        })
    }

    fn method(&self) -> TagMethod {
        TagMethod::Deterministic
    }
}

// ----------------------------------------------------- fabric closed vocab

/// Fabric with the vocabulary in the prompt, for hosts with no classifier key.
/// Unscored, so it never reports `High`.
pub struct FabricClosedVocab {
    canon: CanonicalSet,
    mapping: TagMapping,
    runner: Arc<dyn FabricRunner>,
}

impl FabricClosedVocab {
    pub const PATTERN: &'static str = "obsidian-tags";

    pub fn new(canon: CanonicalSet, mapping: TagMapping, runner: Arc<dyn FabricRunner>) -> Self {
        Self { canon, mapping, runner }
    }

    fn prompt(&self, input: &TagInput) -> String {
        let mut vocab: Vec<&String> = self.canon.all.iter().collect();
        vocab.sort();
        let vocab: Vec<&str> = vocab.into_iter().map(String::as_str).collect();
        format!(
            "VOCABULARY (choose only from this list, at most {} entries):\n{}\n\nTITLE: {}\n\nTEXT:\n{}\n",
            self.canon.max_per_note,
            vocab.join("\n"),
            input.title,
            input.text
        )
    }
}

#[derive(Deserialize)]
struct FabricTagsReply {
    tags: Vec<String>,
}

impl TagClassifier for FabricClosedVocab {
    fn classify(&self, input: &TagInput) -> Result<TagOutput> {
        let raw = self
            .runner
            .run(Self::PATTERN, &self.prompt(input))
            .wrap_err("fabric closed-vocabulary tag call failed")?;
        let reply: FabricTagsReply =
            serde_json::from_str(raw.trim()).wrap_err_with(|| format!("fabric returned non-JSON tags: {raw}"))?;

        // Post-filter: the model is asked for the vocabulary but is not bound
        // to it, so nothing it returns is trusted without matching.
        let mut tags: Vec<String> = Vec::new();
        for candidate in reply
            .tags
            .iter()
            .chain(author_tags(input, &self.canon, &self.mapping).iter())
        {
            for (tag, _) in match_with_tier(candidate, &self.canon, &self.mapping) {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
        tags.sort();
        tags.truncate(self.canon.max_per_note);

        let confidence = if tags.is_empty() { Confidence::Low } else { Confidence::Medium };
        Ok(TagOutput {
            tags,
            scores: None,
            confidence,
            method: TagMethod::FabricClosed,
        })
    }

    fn method(&self) -> TagMethod {
        TagMethod::FabricClosed
    }
}

// ------------------------------------------------------------------- jev

/// TypeSafe's jev model, called directly at `POST /v1/systemone`: one request
/// per note, every canonical tag a `noul` question, each tag's
/// `descriptions:` entry as its `criteria.true` (the best-scoring question
/// shape against the gold tags, per `docs/design/2026-10-16-jev-tag-classifier.md`).
///
/// Holds the RESOLVED key, never the env var name alone, and deliberately
/// derives no `Debug` so the key cannot reach a log line through `{:?}`.
pub struct Jev {
    canon: CanonicalSet,
    mapping: TagMapping,
    threshold: f32,
    api_key_name: String,
    api_key: String,
    model: String,
    endpoint: String,
    timeout: Duration,
}

#[derive(Deserialize)]
struct SystemOneReply {
    model: String,
    answers: HashMap<String, SystemOneAnswer>,
}

#[derive(Deserialize)]
struct SystemOneAnswer {
    #[serde(default)]
    noul: Option<f64>,
}

/// One HTTP exchange, before its status is judged.
struct Exchange {
    status: u16,
    body: String,
    request_id: Option<String>,
    retry_delay: Option<Duration>,
}

/// A 429's wait: `retry-after-ms` (integer milliseconds), else `Retry-After`
/// (integer seconds), whichever parses first. `None` when neither does, which
/// the caller treats as "do not retry".
fn retry_delay(retry_after_ms: Option<&str>, retry_after: Option<&str>) -> Option<Duration> {
    retry_after_ms
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .or_else(|| {
            retry_after
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs)
        })
}

/// The error for a non-200 answer. Carries the status, `detail.error_type`,
/// `detail.message` and the request id, never the request body or the key.
///
/// A 422 is FastAPI's validation body, whose `detail` is a list of
/// `{type, loc, msg, input}` and whose `input` echoes the whole request; only
/// `loc` and `msg` survive. A body that is not JSON contributes nothing.
fn api_error(status: u16, body: &str, request_id: Option<&str>) -> eyre::Report {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let detail = parsed.as_ref().and_then(|v| v.get("detail"));
    let (error_type, message) = match detail {
        Some(serde_json::Value::Object(d)) => (
            d.get("error_type").and_then(|v| v.as_str()).map(str::to_string),
            d.get("message").and_then(|v| v.as_str()).map(str::to_string),
        ),
        Some(serde_json::Value::Array(items)) => {
            let messages: Vec<String> = items
                .iter()
                .map(|item| {
                    let loc: Vec<String> = item
                        .get("loc")
                        .and_then(|l| l.as_array())
                        .map(|l| {
                            l.iter()
                                .map(|p| p.as_str().map(str::to_string).unwrap_or_else(|| p.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let msg = item.get("msg").and_then(|m| m.as_str()).unwrap_or("invalid");
                    format!("{}: {msg}", loc.join("."))
                })
                .collect();
            (Some("validation_error".to_string()), Some(messages.join("; ")))
        }
        _ => (None, None),
    };
    eyre!(
        "jev returned {status}: error_type={} message={} request_id={}",
        error_type.as_deref().unwrap_or("unknown"),
        message.as_deref().unwrap_or("none"),
        request_id.unwrap_or("none")
    )
}

impl Jev {
    /// Resolve `cfg.api-key` once, here. A name that does not resolve leaves
    /// the key empty, and every `classify` then errors before any request.
    pub fn new(canon: CanonicalSet, mapping: TagMapping, cfg: &TagsClassifierConfig) -> Self {
        let api_key = vault::config::resolve_secret(&cfg.api_key)
            .map(|k| k.trim().to_string())
            .unwrap_or_default();
        log::debug!(
            "Jev::new: api_key={} resolved={} model={} timeout_secs={}",
            cfg.api_key,
            !api_key.is_empty(),
            cfg.model,
            cfg.timeout_secs
        );
        Self {
            canon,
            mapping,
            threshold: cfg.threshold,
            api_key_name: cfg.api_key.clone(),
            api_key,
            model: cfg.model.clone(),
            endpoint: format!("{}{SYSTEMONE_PATH}", Self::base_url(cfg)),
            timeout: Duration::from_secs(cfg.timeout_secs),
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    fn base_url(cfg: &TagsClassifierConfig) -> String {
        cfg.base_url
            .as_deref()
            .map(|b| b.trim_end_matches('/').to_string())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
    }

    #[cfg(not(any(test, feature = "test-util")))]
    fn base_url(_cfg: &TagsClassifierConfig) -> String {
        DEFAULT_BASE_URL.to_string()
    }

    fn sorted_vocabulary(&self) -> Vec<String> {
        let mut v: Vec<String> = self.canon.all.iter().cloned().collect();
        v.sort();
        v
    }

    /// One `noul` question per canonical tag. A tag with a description gets
    /// it as `criteria.true`; a tag without one is asked by name alone.
    fn request_body(&self, state: &str) -> serde_json::Value {
        let mut questions = serde_json::Map::new();
        for tag in self.sorted_vocabulary() {
            let mut question = serde_json::json!({
                "type": "noul",
                "instructions": format!("Is this text about {tag}?"),
            });
            if let Some(description) = self.canon.description.get(&tag) {
                question["criteria"] = serde_json::json!({ "true": description });
            }
            questions.insert(tag, question);
        }
        serde_json::json!({
            "model": self.model,
            "state": state,
            "questions": questions,
        })
    }

    /// One POST bounded by what is left of `deadline`. Transport errors and
    /// timeouts are `Err`; every HTTP status comes back for the caller to judge.
    fn send(&self, body: &serde_json::Value, deadline: Instant) -> Result<Exchange> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(eyre!("jev: the {:?} deadline ran out before the request", self.timeout));
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(remaining))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut response = agent
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .with_context(|| format!("jev request to {} failed", self.endpoint))?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let request_id = header("x-typesafe-request-id");
        let retry_delay = retry_delay(header("retry-after-ms").as_deref(), header("retry-after").as_deref());
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .context("failed to read the jev response body")?;
        log::debug!(
            "Jev::send: status={status} request_id={} bytes={}",
            request_id.as_deref().unwrap_or("none"),
            text.len()
        );
        Ok(Exchange {
            status,
            body: text,
            request_id,
            retry_delay,
        })
    }

    /// POST, retrying one 429 when its advertised delay plus a request still
    /// fits inside `deadline`. A 429 with no usable delay, one whose delay
    /// does not fit, or a second 429 is `Err`.
    fn post(&self, body: &serde_json::Value, deadline: Instant) -> Result<String> {
        let first = self.send(body, deadline)?;
        if first.status != 429 {
            return Self::accept(first);
        }
        let request_id = first.request_id.as_deref().unwrap_or("none");
        let Some(delay) = first.retry_delay else {
            return Err(eyre!(
                "jev returned 429 with no usable retry-after-ms or Retry-After header: request_id={request_id}"
            ));
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if delay + RETRY_REQUEST_ALLOWANCE > remaining {
            return Err(eyre!(
                "jev returned 429 asking for {delay:?}, which does not fit the {remaining:?} left of the deadline: request_id={request_id}"
            ));
        }
        log::debug!("Jev::post: 429, retrying once after {delay:?} (request_id={request_id})");
        std::thread::sleep(delay);
        let second = self.send(body, deadline)?;
        if second.status == 429 {
            return Err(eyre!(
                "jev returned 429 again after one retry: request_id={}",
                second.request_id.as_deref().unwrap_or("none")
            ));
        }
        Self::accept(second)
    }

    fn accept(exchange: Exchange) -> Result<String> {
        if exchange.status == 200 {
            return Ok(exchange.body);
        }
        Err(api_error(
            exchange.status,
            &exchange.body,
            exchange.request_id.as_deref(),
        ))
    }

    /// The per-tag scores, one per canonical tag. Malformed JSON, a tag we
    /// asked about but got no answer for, a `noul` outside `[0, 1]`, or an
    /// answer from a non-jev model is `Err`. Keys we did not ask for drop.
    fn scores(&self, text: &str) -> Result<Vec<(String, f32)>> {
        let reply: SystemOneReply =
            serde_json::from_str(text).map_err(|e| eyre!("jev returned malformed JSON ({} bytes): {e}", text.len()))?;
        if !reply.model.starts_with("jev") {
            return Err(eyre!(
                "jev answer came from model {:?}, not jev; refusing its scores",
                reply.model
            ));
        }
        log::debug!("Jev::scores: served model={}", reply.model);
        let mut scores = Vec::with_capacity(self.canon.all.len());
        for tag in self.sorted_vocabulary() {
            let answer = reply
                .answers
                .get(&tag)
                .ok_or_else(|| eyre!("jev answer is missing tag {tag:?}"))?;
            let noul = answer
                .noul
                .ok_or_else(|| eyre!("jev answer for tag {tag:?} carries no noul score"))?;
            if !(0.0..=1.0).contains(&noul) {
                return Err(eyre!("jev noul {noul} for tag {tag:?} is outside [0, 1]"));
            }
            scores.push((tag, noul as f32));
        }
        Ok(scores)
    }

    fn finish(&self, input: &TagInput, scores: &[(String, f32)]) -> TagOutput {
        let mut selected = select_above_threshold(scores, self.threshold, self.canon.max_per_note);

        // Author candidates join at tier 0, above anything the model scored.
        for tag in author_tags(input, &self.canon, &self.mapping) {
            if !selected.iter().any(|(t, _)| *t == tag) {
                selected.insert(0, (tag, 1.0));
            }
        }
        selected.truncate(self.canon.max_per_note);

        // Guard the subset invariant: the vocabulary is what we sent, but a
        // service that echoes an unknown label must not reach the vault.
        selected.retain(|(t, _)| self.canon.all.contains(t));

        let confidence = if selected.is_empty() { Confidence::Low } else { Confidence::High };
        TagOutput {
            tags: selected.iter().map(|(t, _)| t.clone()).collect(),
            scores: Some(selected),
            confidence,
            method: TagMethod::Jev,
        }
    }
}

impl TagClassifier for Jev {
    fn classify(&self, input: &TagInput) -> Result<TagOutput> {
        log::debug!(
            "Jev::classify: title_len={} text_len={} candidates={}",
            input.title.len(),
            input.text.len(),
            input.candidates.len()
        );
        if self.api_key.is_empty() {
            return Err(eyre!(
                "jev: no TypeSafe key; {} resolves empty, so no request was sent",
                self.api_key_name
            ));
        }
        // Empty text is a guaranteed 422 that would degrade the receipt, so
        // no request: author tags only, `Low` if there are none.
        if input.text.trim().is_empty() {
            log::debug!("Jev::classify: empty text, no request");
            return Ok(self.finish(input, &[]));
        }

        let deadline = Instant::now() + self.timeout;
        let body = self.request_body(&format!("{}\n\n{}", input.title, input.text));
        let text = self.post(&body, deadline)?;
        let scores = self.scores(&text)?;
        let out = self.finish(input, &scores);
        log::debug!("Jev::classify: tags={:?} confidence={:?}", out.tags, out.confidence);
        Ok(out)
    }

    fn method(&self) -> TagMethod {
        TagMethod::Jev
    }
}

// ------------------------------------------------------------ test double

#[cfg(test)]
/// Returns canned tags, post-filtered through the vocabulary like the real
/// impls, so a test double can never produce a non-canonical tag either.
pub struct FakeTagClassifier {
    canon: CanonicalSet,
    mapping: TagMapping,
    canned: Vec<String>,
    fail: bool,
}

#[cfg(test)]
impl FakeTagClassifier {
    pub fn new(canon: CanonicalSet, mapping: TagMapping, canned: Vec<String>) -> Self {
        Self {
            canon,
            mapping,
            canned,
            fail: false,
        }
    }

    /// A classifier that always errors, for the fallback and degraded-receipt paths.
    pub fn failing(canon: CanonicalSet, mapping: TagMapping) -> Self {
        Self {
            canon,
            mapping,
            canned: Vec::new(),
            fail: true,
        }
    }
}

#[cfg(test)]
impl TagClassifier for FakeTagClassifier {
    fn classify(&self, input: &TagInput) -> Result<TagOutput> {
        if self.fail {
            return Err(eyre!("FakeTagClassifier: forced failure"));
        }
        let mut tags: Vec<String> = Vec::new();
        for candidate in self
            .canned
            .iter()
            .chain(author_tags(input, &self.canon, &self.mapping).iter())
        {
            for (tag, _) in match_with_tier(candidate, &self.canon, &self.mapping) {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
        tags.sort();
        tags.truncate(self.canon.max_per_note);
        let confidence = if tags.is_empty() { Confidence::Low } else { Confidence::High };
        Ok(TagOutput {
            tags,
            scores: None,
            confidence,
            method: TagMethod::Deterministic,
        })
    }

    fn method(&self) -> TagMethod {
        TagMethod::Deterministic
    }
}
