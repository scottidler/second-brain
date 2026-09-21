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
use std::time::Duration;

use eyre::{Context, Result, eyre};
use serde::{Deserialize, Serialize};
use vault::canonical::{self, CanonicalSet, TagMapping};

#[cfg(test)]
mod tests;

/// The API's hard ceiling on labels per call (Phase 0, observed in the tool
/// schema 2026-09-20). The vocabulary is larger, so production always shards.
pub const MAX_LABELS_PER_CALL: usize = 100;

const DEFAULT_ENDPOINT: &str = "https://classifier.dev/v1/classify";

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
/// verbatim to classifier.dev, so an untruncated body means a whole
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
    ClassifierDev,
}

impl TagMethod {
    /// Written to `cortex-classified-by`.
    pub fn as_str(&self) -> &'static str {
        match self {
            TagMethod::Deterministic => "deterministic",
            TagMethod::FabricClosed => "fabric-closed",
            TagMethod::ClassifierDev => "classifier-dev",
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

    /// Overridden by `ClassifierDev`, which sends up to 1,000 texts per call.
    fn classify_batch(&self, inputs: &[TagInput]) -> Vec<Result<TagOutput>> {
        inputs.iter().map(|i| self.classify(i)).collect()
    }

    fn method(&self) -> TagMethod;
}

// ---------------------------------------------------------------- config

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClassifierKind {
    #[default]
    Deterministic,
    FabricClosed,
    ClassifierDev,
}

/// The `tags.classifier` block, identical in `borg.yml` and `cortex.yml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct TagsClassifierConfig {
    pub classifier: ClassifierKind,
    pub threshold: f32,
    /// NAME of the env var holding the classifier.dev key, never the key.
    pub api_key_env: String,
    /// Runs when the selected impl errors. Borg marks the receipt degraded;
    /// cortex holds the note only if the fallback also returns `Low`.
    pub fallback: ClassifierKind,
    pub endpoint: String,
    pub timeout_secs: u64,
}

impl Default for TagsClassifierConfig {
    fn default() -> Self {
        Self {
            classifier: ClassifierKind::Deterministic,
            threshold: 0.9,
            api_key_env: "CLASSIFY_API_KEY".to_string(),
            fallback: ClassifierKind::Deterministic,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            timeout_secs: 30,
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
        ClassifierKind::ClassifierDev => Box::new(ClassifierDev::new(canon.clone(), mapping.clone(), cfg)),
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

/// Split a sorted label list into shards of at most `MAX_LABELS_PER_CALL`,
/// **evenly**: Phase 0b validated a 59/58 partition and panel r4 OQ8 pins it,
/// because a greedy 100/17 split is an untested co-label context. Re-run the
/// shard-invariance trial before the vocabulary passes ~200 labels.
pub fn shard_labels(sorted_labels: &[String]) -> Vec<Vec<String>> {
    if sorted_labels.is_empty() {
        return Vec::new();
    }
    let shards = sorted_labels.len().div_ceil(MAX_LABELS_PER_CALL);
    let per = sorted_labels.len().div_ceil(shards);
    sorted_labels.chunks(per).map(<[String]>::to_vec).collect()
}

/// Merge per-shard score maps into one ranking. Valid because the API returns
/// independent per-label probabilities rather than a softmax, measured in
/// Phase 0b: re-partitioning moves scores no more than two identical calls do.
pub fn merge_shard_scores(shard_scores: &[HashMap<String, f32>]) -> Vec<(String, f32)> {
    let mut merged: HashMap<String, f32> = HashMap::new();
    for shard in shard_scores {
        for (label, score) in shard {
            let slot = merged.entry(label.clone()).or_insert(*score);
            if *score > *slot {
                *slot = *score;
            }
        }
    }
    merged.into_iter().collect()
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
/// `work`/`life`/`homelab`/`diy`/`writing` values from note text (Phase 0b).
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

// ---------------------------------------------------------- classifier.dev

/// Multi-label classifier.dev over the sorted vocabulary, sharded.
pub struct ClassifierDev {
    canon: CanonicalSet,
    mapping: TagMapping,
    threshold: f32,
    api_key_env: String,
    endpoint: String,
    timeout: Duration,
}

#[derive(Deserialize)]
struct ClassifyResult {
    #[serde(default)]
    scores: HashMap<String, f32>,
}

#[derive(Deserialize)]
struct ClassifyResponse {
    #[serde(default)]
    results: Vec<ClassifyResult>,
}

impl ClassifierDev {
    pub fn new(canon: CanonicalSet, mapping: TagMapping, cfg: &TagsClassifierConfig) -> Self {
        Self {
            canon,
            mapping,
            threshold: cfg.threshold,
            api_key_env: cfg.api_key_env.clone(),
            endpoint: cfg.endpoint.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs),
        }
    }

    fn sorted_vocabulary(&self) -> Vec<String> {
        let mut v: Vec<String> = self.canon.all.iter().cloned().collect();
        v.sort();
        v
    }

    fn post(&self, labels: &[String], texts: &[String]) -> Result<ClassifyResponse> {
        let api_key = std::env::var(&self.api_key_env)
            .with_context(|| format!("environment variable {} is not set", self.api_key_env))?;
        let body = serde_json::json!({
            "inputs": texts,
            "labels": labels,
            "multi_label": true,
            "max_labels": self.canon.max_per_note,
        });
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut response = agent
            .post(&self.endpoint)
            .header("authorization", &format!("Bearer {api_key}"))
            .header("content-type", "application/json")
            .send_json(&body)
            .context("classifier.dev request failed")?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .context("failed to read classifier.dev response body")?;
        if status != 200 {
            return Err(eyre!("classifier.dev returned {status}: {text}"));
        }
        serde_json::from_str(&text).wrap_err_with(|| format!("failed to parse classifier.dev response: {text}"))
    }

    /// Shared by `classify` and `classify_batch`: one call per shard over the
    /// whole batch, merged per input. Any shard failing fails the batch, so a
    /// partial vocabulary never produces a tag set that looks complete.
    fn classify_texts(&self, inputs: &[TagInput]) -> Result<Vec<TagOutput>> {
        let texts: Vec<String> = inputs.iter().map(|i| format!("{}\n\n{}", i.title, i.text)).collect();
        let shards = shard_labels(&self.sorted_vocabulary());
        log::debug!(
            "ClassifierDev::classify_texts: texts={} shards={} threshold={}",
            texts.len(),
            shards.len(),
            self.threshold
        );

        let mut per_input: Vec<Vec<HashMap<String, f32>>> = vec![Vec::new(); inputs.len()];
        for shard in &shards {
            let response = self.post(shard, &texts)?;
            if response.results.len() != inputs.len() {
                return Err(eyre!(
                    "classifier.dev returned {} results for {} inputs",
                    response.results.len(),
                    inputs.len()
                ));
            }
            for (slot, result) in per_input.iter_mut().zip(response.results) {
                slot.push(result.scores);
            }
        }

        Ok(inputs
            .iter()
            .zip(per_input)
            .map(|(input, shard_scores)| self.finish(input, &shard_scores))
            .collect())
    }

    fn finish(&self, input: &TagInput, shard_scores: &[HashMap<String, f32>]) -> TagOutput {
        let merged = merge_shard_scores(shard_scores);
        let mut selected = select_above_threshold(&merged, self.threshold, self.canon.max_per_note);

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
            method: TagMethod::ClassifierDev,
        }
    }
}

impl TagClassifier for ClassifierDev {
    fn classify(&self, input: &TagInput) -> Result<TagOutput> {
        let mut out = self.classify_texts(std::slice::from_ref(input))?;
        out.pop()
            .ok_or_else(|| eyre!("classifier.dev returned no result for a single input"))
    }

    fn classify_batch(&self, inputs: &[TagInput]) -> Vec<Result<TagOutput>> {
        match self.classify_texts(inputs) {
            Ok(outputs) => outputs.into_iter().map(Ok).collect(),
            Err(e) => {
                // Whole-batch failure: one shard short means every input's
                // ranking is missing part of the vocabulary.
                inputs
                    .iter()
                    .map(|_| Err(eyre!("classifier.dev batch failed: {e}")))
                    .collect()
            }
        }
    }

    fn method(&self) -> TagMethod {
        TagMethod::ClassifierDev
    }
}

// ------------------------------------------------------------ test double

/// Returns canned tags, post-filtered through the vocabulary like the real
/// impls, so a test double can never produce a non-canonical tag either.
pub struct FakeTagClassifier {
    canon: CanonicalSet,
    mapping: TagMapping,
    canned: Vec<String>,
    fail: bool,
}

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
