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

pub mod bulk;
pub mod spend;

#[cfg(test)]
mod tests;

/// The API's hard ceiling on labels per call (Phase 0, observed in the tool
/// schema 2026-09-20). The vocabulary is larger, so production always shards.
pub const MAX_LABELS_PER_CALL: usize = 100;

/// The keyless (public) tier's ceiling on texts per call. An authenticated
/// workspace key raises it to 1,000, but the default path is keyless, so every
/// batch chunks at the lower number: one limit, no branch, and a batch that is
/// correct whether or not a token is configured.
pub const MAX_TEXTS_PER_CALL: usize = 200;

/// Estimated input tokens one classifier.dev request may carry, from the
/// largest call Phase 0 saw pass: 53 notes against the decomposed labels,
/// 157,708 tokens, $0.0066 (design doc, Phase 0 Results). The free tier refuses
/// any request over $0.01 (`402 request_spending_limit`; 159 bare-label notes,
/// about 300k tokens, did), and that cap is not published in tokens, so the
/// budget is the measured pass rather than an extrapolation toward the cap.
/// A code constant, not a knob: it describes the service, and a 402 still
/// splits the chunk if the service moves.
const REQUEST_TOKEN_BUDGET: f64 = 157_708.0;

/// Phase 0's least-squares cost model over 46 calls (max relative error
/// 1.7%): tokens per note per call = `TOKENS_PER_NOTE` + `TOKENS_PER_TEXT_CHAR`
/// x wire-text chars + questions (labels sent) x `TOKENS_PER_QUESTION` +
/// `TOKENS_PER_LABEL_CHAR` x the chars of every label sent. Every fitted call
/// had 53 notes, so the per-note and per-question terms are not separately
/// identified; their sum over a real shard is what the calls measured.
const TOKENS_PER_NOTE: f64 = 26.9;
const TOKENS_PER_TEXT_CHAR: f64 = 0.43;
const TOKENS_PER_QUESTION: f64 = 24.7;
const TOKENS_PER_LABEL_CHAR: f64 = 0.21;

/// classifier.dev's price per input token as Phase 0 read it off
/// `pricing.estimated_usd`: bare labels 99,952 tokens for $0.0042 and
/// described labels 149,070 for $0.0063, both $0.042 per million. Only our
/// estimates use it; the spend ledger records what the service reported.
const USD_PER_INPUT_TOKEN: f64 = 0.042 / 1_000_000.0;

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
/// the note body. Neither leaves the machine as is: every outbound path builds
/// its text through `clip_classifier_text`, which bounds the assembled string.
#[derive(Debug, Clone, Copy)]
pub struct TagInput<'a> {
    pub title: &'a str,
    pub text: &'a str,
    pub candidates: &'a [TagCandidate],
}

/// The most characters of note text any classifier sends off the machine
/// (design doc G6, API Design).
///
/// This is a data-boundary number, not a tuning knob: the wire text is shipped
/// verbatim to classifier.dev, so an unbounded one means a whole transcript of
/// captured audio, or a 129,928-char summary, leaves the machine. Phase 0 kept
/// 900: clipping cost 0.006 F1 against unclipped, inside the 0.02 bar.
pub const CLASSIFIER_TEXT_CHARS: usize = 900;

/// The only builder of classifier wire text: `title + "\n\n" + text`, clipped
/// as one string at `CLASSIFIER_TEXT_CHARS`. Clipping the assembled string,
/// not `text` alone, is what makes the bound hold for the exact bytes sent; a
/// long title eats into the text's share. Counts characters rather than bytes
/// so a multi-byte boundary cannot panic.
pub fn clip_classifier_text(title: &str, text: &str) -> String {
    format!("{title}\n\n{text}")
        .chars()
        .take(CLASSIFIER_TEXT_CHARS)
        .collect()
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

    /// Overridden by `ClassifierDev`, which sends many texts per call.
    fn classify_batch(&self, inputs: &[TagInput]) -> Vec<Result<TagOutput>> {
        inputs.iter().map(|i| self.classify(i)).collect()
    }

    /// `classify_batch` plus what its calls cost, for the bulk paths that
    /// write the spend ledger. Implementations that spend nothing on
    /// classifier.dev report no calls.
    fn classify_batch_metered(&self, inputs: &[TagInput]) -> MeteredBatch {
        MeteredBatch {
            outputs: self.classify_batch(inputs),
            calls: Vec::new(),
            service_limit: None,
        }
    }

    /// What classifying `inputs` would cost on classifier.dev, or `None` for
    /// an implementation that does not call it.
    fn estimate(&self, _inputs: &[TagInput]) -> Option<SpendEstimate> {
        None
    }

    fn method(&self) -> TagMethod;
}

/// `classify_batch`'s outputs with the usage rows of the calls behind them.
#[derive(Debug)]
pub struct MeteredBatch {
    pub outputs: Vec<Result<TagOutput>>,
    pub calls: Vec<CallUsage>,
    /// Set when classifier.dev refused a call over a limit a smaller request
    /// cannot fix (see `ScoredBatch::service_limit`).
    pub service_limit: Option<String>,
}

/// Our estimate of what scoring each input costs, from Phase 0's token model.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendEstimate {
    /// One entry per input, in input order.
    pub per_note_usd: Vec<f64>,
}

impl SpendEstimate {
    pub fn total_usd(&self) -> f64 {
        self.per_note_usd.iter().sum()
    }

    /// How many inputs, taken in order, fit inside `limit_usd`.
    pub fn notes_within(&self, limit_usd: f64) -> usize {
        let mut total = 0.0;
        self.per_note_usd
            .iter()
            .take_while(|usd| {
                total += *usd;
                total <= limit_usd
            })
            .count()
    }
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
    /// NAME of the env var holding an OPTIONAL classifier.dev workspace token,
    /// never the token itself. Empty (the default) means the keyless public
    /// tier, which is what classifier.dev serves without credentials.
    ///
    /// Deliberately not the old `api-key-env` / `CLASSIFY_API_KEY` pair. That
    /// key was rejected upstream (`401 invalid_api_key`) while still being
    /// present in every shell and in the secrets manifest, so every classify
    /// fell back to `deterministic` and nothing said so. A fresh name means a
    /// stale value cannot quietly reintroduce that state: set this only when
    /// a workspace key from classifier.dev/app/keys is actually wanted.
    pub token_env: String,
    /// Runs when the selected impl errors. Borg marks the receipt degraded;
    /// cortex holds the note only if the fallback also returns `Low`.
    pub fallback: ClassifierKind,
    pub endpoint: String,
    pub timeout_secs: u64,
    /// classifier.dev's free spend per IP per UTC day (`llms.txt`). Bulk runs
    /// (more than one note) spend against it through the spend ledger.
    pub daily_usd_budget: f64,
    /// Headroom a bulk run leaves in the day for borg's live ingest, which
    /// does not write the ledger (about 20 notes x 2 shards a day).
    pub ingest_reserve_usd: f64,
    /// Appended after classifier.dev's fixed rubric ("A category applies if
    /// the text meaningfully touches it, even briefly.") on every question.
    /// Empty (the default) sends no `instructions` field: Phase 0's A3 arm
    /// (rubric-tightening instructions) did not beat baseline, so nothing
    /// ships by default. A host may still set one.
    pub instructions: String,
    /// How many times each shard is called, so `score_batch` can select on
    /// the mean rather than a single call. Default 3 (design doc Phase 0
    /// Results, repeat mean): on the baseline winner's six calls, mean-of-3
    /// instability (0.170) beat single-call (0.195) and mean-of-3 F1 (0.743)
    /// did not fall below single-call F1 (0.741). Every repeat costs a full
    /// call, so this multiplies both ingest and bulk spend by `repeats`.
    pub repeats: u8,
}

impl Default for TagsClassifierConfig {
    fn default() -> Self {
        Self {
            classifier: ClassifierKind::Deterministic,
            threshold: 0.9,
            token_env: String::new(),
            fallback: ClassifierKind::Deterministic,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            timeout_secs: 30,
            daily_usd_budget: 0.50,
            ingest_reserve_usd: 0.10,
            instructions: String::new(),
            repeats: 3,
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

/// The wire label sent for each slug of one shard (`slug: description` when
/// the vocabulary carries one, else the bare slug), paired with the slug it
/// maps back to.
///
/// Sorted by **slug**, not by the wire string: `shard_labels` fixes the
/// pinned 59/58 partition on the bare, slug-sorted vocabulary (Phase 0b,
/// panel r4 OQ8), and sorting the described strings instead would move `ai:`
/// after `ai-safety`, an untested co-label context. `score_batch`'s
/// missing/unknown-key rule (`check_shard_scores`) covers the mapping back:
/// a label the service doesn't echo, or echoes under a key we never sent,
/// fails that note rather than silently landing on the wrong slug.
pub fn labels_for_shard(shard: &[String], descriptions: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = shard
        .iter()
        .map(|slug| {
            let wire = match descriptions.get(slug) {
                Some(description) => format!("{slug}: {description}"),
                None => slug.clone(),
            };
            (wire, slug.clone())
        })
        .collect();
    pairs.sort_by(|a, b| a.1.cmp(&b.1));
    pairs
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
        // The title leads the clipped text rather than sitting on its own
        // line, so this prompt carries the same bounded string classifier.dev
        // gets and a summary-less transcript never reaches Fabric whole.
        format!(
            "VOCABULARY (choose only from this list, at most {} entries):\n{}\n\nTEXT:\n{}\n",
            self.canon.max_per_note,
            vocab.join("\n"),
            clip_classifier_text(input.title, input.text)
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
    token_env: String,
    endpoint: String,
    instructions: String,
    repeats: u8,
    transport: Arc<dyn Transport>,
}

/// Every slug's score for each input, or why that input has none, plus what
/// each HTTP call cost.
#[derive(Debug)]
pub struct ScoredBatch {
    /// One entry per input, in input order. `Ok` holds a score for every
    /// vocabulary slug; anything less is that note's `Err`, never a partial map.
    pub notes: Vec<Result<HashMap<String, f32>>>,
    /// One row per HTTP call made, successful or not, in call order.
    pub calls: Vec<CallUsage>,
    /// classifier.dev's answer when it refused a call over a limit that a
    /// smaller request cannot fix: a 429 or a 402 other than
    /// `request_spending_limit`, which is the per-IP day cap. Once set, no
    /// further call was made, and every note not yet scored is `Err`.
    pub service_limit: Option<String>,
}

/// What one classifier.dev call reported about its own cost. Both numbers
/// are the service's (`usage.input_tokens`, `pricing.estimated_usd`), not our
/// estimate; a failed call reports zero because no response carried them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallUsage {
    pub input_tokens: u64,
    pub estimated_usd: f64,
    pub ok: bool,
}

impl CallUsage {
    fn failed() -> Self {
        Self {
            input_tokens: 0,
            estimated_usd: 0.0,
            ok: false,
        }
    }
}

#[derive(Deserialize)]
struct ClassifyResult {
    #[serde(default)]
    scores: HashMap<String, f32>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
}

#[derive(Deserialize, Default)]
struct Pricing {
    #[serde(default)]
    estimated_usd: f64,
}

#[derive(Deserialize)]
struct ClassifyResponse {
    #[serde(default)]
    results: Vec<ClassifyResult>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    pricing: Option<Pricing>,
}

impl ClassifyResponse {
    fn call_usage(&self) -> CallUsage {
        CallUsage {
            input_tokens: self.usage.as_ref().map_or(0, |u| u.input_tokens),
            estimated_usd: self.pricing.as_ref().map_or(0.0, |p| p.estimated_usd),
            ok: true,
        }
    }
}

/// Sync port over one classifier.dev HTTP exchange: the JSON body out, the
/// status and raw body back. Status handling stays in `ClassifierDev::post`
/// so the fake responder in tests exercises the same path production does.
/// Public so a bulk caller in another crate (`cortex::tag_apply`) can test
/// against a fake service through `ClassifierDev::with_transport`.
pub trait Transport: Send + Sync {
    fn send(&self, body: &serde_json::Value, token: Option<&str>) -> Result<(u16, String)>;
}

struct UreqTransport {
    endpoint: String,
    timeout: Duration,
}

impl Transport for UreqTransport {
    fn send(&self, body: &serde_json::Value, token: Option<&str>) -> Result<(u16, String)> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let mut request = agent.post(&self.endpoint).header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", &format!("Bearer {token}"));
        }
        let mut response = request.send_json(body).context("classifier.dev request failed")?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .context("failed to read classifier.dev response body")?;
        Ok((status, text))
    }
}

/// How one call ended. The per-request spend refusal is its own arm because
/// it is the one failure a smaller request fixes.
enum CallOutcome {
    Scored(ClassifyResponse),
    /// HTTP 402 `request_spending_limit`: this request alone is over the free
    /// tier's per-request cap. Not the per-day cap, which splitting cannot fix.
    OverRequestLimit(String),
    /// HTTP 429, or a 402 that is not `request_spending_limit`: a per-IP
    /// minute, day, or spend limit. Every later call would meet it too.
    ServiceLimit(String),
    Failed(eyre::Report),
}

/// `multi` is classifier.dev's multi-label switch; `multi_label` is not a field
/// it reads, and only worked because a numeric `max_labels` also enables it.
///
/// `instructions` is omitted entirely when empty (the code default), rather
/// than sent as `""`: an empty request body is byte-identical to what today's
/// bare, undescribed vocabulary already sends, so shipping this code changes
/// nothing on the wire until a description or an instructions string is set.
fn request_body(labels: &[String], texts: &[String], max_labels: usize, instructions: &str) -> serde_json::Value {
    let mut body = serde_json::json!({
        "inputs": texts,
        "labels": labels,
        "multi": true,
        "max_labels": max_labels,
    });
    if !instructions.is_empty() {
        body["instructions"] = serde_json::json!(instructions);
    }
    body
}

/// classifier.dev answers from a non-Jev LLM when Jev fails (`ling-*`,
/// `mercury-*`), and those scores are not comparable to Jev's. Such an answer,
/// or one naming no model, fails the call so the fallback runs and the ingest
/// is marked degraded instead of accepting it as a Jev answer.
fn ensure_jev(response: &ClassifyResponse) -> Result<()> {
    for (i, result) in response.results.iter().enumerate() {
        match result.model.as_deref() {
            Some(model) if model.starts_with("jev") => {
                log::debug!("ensure_jev: result {i} answered by {model}");
            }
            other => {
                return Err(eyre!(
                    "classifier.dev result {i} was answered by {other:?}, not Jev; refusing non-Jev scores"
                ));
            }
        }
    }
    Ok(())
}

/// Estimated input tokens one note costs in a call over `labels`, from the
/// Phase 0 cost model (see `REQUEST_TOKEN_BUDGET`).
fn note_tokens(text_chars: usize, labels: &[String]) -> f64 {
    let label_chars: usize = labels.iter().map(|l| l.chars().count()).sum();
    TOKENS_PER_NOTE
        + TOKENS_PER_TEXT_CHAR * text_chars as f64
        + TOKENS_PER_QUESTION * labels.len() as f64
        + TOKENS_PER_LABEL_CHAR * label_chars as f64
}

/// Split notes (given by wire-text length) into consecutive chunk sizes, each
/// estimated under `REQUEST_TOKEN_BUDGET` and at most `MAX_TEXTS_PER_CALL`.
/// A chunk always holds at least one note: one note over budget is still
/// sent, and the service's 402 decides it.
fn plan_chunks(text_chars: &[usize], labels: &[String]) -> Vec<usize> {
    let mut chunks = Vec::new();
    let (mut count, mut tokens) = (0usize, 0f64);
    for &chars in text_chars {
        let cost = note_tokens(chars, labels);
        if count > 0 && (count == MAX_TEXTS_PER_CALL || tokens + cost > REQUEST_TOKEN_BUDGET) {
            chunks.push(count);
            (count, tokens) = (0, 0.0);
        }
        count += 1;
        tokens += cost;
    }
    if count > 0 {
        chunks.push(count);
    }
    chunks
}

/// Lowercase hex SHA-256: the cursor's config and wire-text digests, which
/// must stay stable across builds (std's `DefaultHasher` does not promise to).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A note's scores from one shard, accepted only when they cover exactly the
/// labels sent: a missing label would read as a zero and an unknown key would
/// be a score for something we never asked about.
fn check_shard_scores(sent: &[String], scores: HashMap<String, f32>) -> Result<HashMap<String, f32>> {
    if let Some(unknown) = scores.keys().find(|k| !sent.contains(k)) {
        return Err(eyre!(
            "classifier.dev returned a score for {unknown:?}, which was not a label sent"
        ));
    }
    if let Some(missing) = sent.iter().find(|l| !scores.contains_key(*l)) {
        return Err(eyre!("classifier.dev returned no score for the sent label {missing:?}"));
    }
    Ok(scores)
}

impl ClassifierDev {
    pub fn new(canon: CanonicalSet, mapping: TagMapping, cfg: &TagsClassifierConfig) -> Self {
        let transport = UreqTransport {
            endpoint: cfg.endpoint.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs),
        };
        Self::with_transport(canon, mapping, cfg, Arc::new(transport))
    }

    pub fn with_transport(
        canon: CanonicalSet,
        mapping: TagMapping,
        cfg: &TagsClassifierConfig,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            canon,
            mapping,
            threshold: cfg.threshold,
            token_env: cfg.token_env.clone(),
            endpoint: cfg.endpoint.clone(),
            instructions: cfg.instructions.clone(),
            // A misconfigured 0 reads as 1: zero calls would silently score
            // nothing rather than failing loud, and a bulk caller's cost
            // estimate (`wire_usd`) assumes at least one call per shard too.
            repeats: cfg.repeats.max(1),
            transport,
        }
    }

    fn sorted_vocabulary(&self) -> Vec<String> {
        let mut v: Vec<String> = self.canon.all.iter().cloned().collect();
        v.sort();
        v
    }

    /// Every shard's wire labels paired with the slug each maps back to, in
    /// the fixed 59/58 partition. The one place `labels_for_shard` is called
    /// against this vocabulary's descriptions, so every caller (scoring, the
    /// cost estimate, the config hash) sees the same shards.
    fn label_shards(&self) -> Vec<Vec<(String, String)>> {
        shard_labels(&self.sorted_vocabulary())
            .iter()
            .map(|shard| labels_for_shard(shard, &self.canon.descriptions))
            .collect()
    }

    /// The optional workspace token, resolved from the configured env var.
    ///
    /// `None` on the keyless path, which is the default: no `token-env` named,
    /// or the named variable absent or empty. A NAMED-but-unset variable is
    /// not an error, because falling through to the public tier is a working
    /// request, not a broken one.
    fn token(&self) -> Option<String> {
        if self.token_env.is_empty() {
            return None;
        }
        match std::env::var(&self.token_env) {
            Ok(v) if !v.trim().is_empty() => Some(v),
            _ => {
                log::debug!(
                    "ClassifierDev::token: {} names no value; using the keyless tier",
                    self.token_env
                );
                None
            }
        }
    }

    fn post(&self, labels: &[String], texts: &[String]) -> CallOutcome {
        let body = request_body(labels, texts, self.canon.max_per_note, &self.instructions);
        let (status, text) = match self.transport.send(&body, self.token().as_deref()) {
            Ok(reply) => reply,
            Err(e) => return CallOutcome::Failed(e),
        };
        if status == 402 && text.contains("request_spending_limit") {
            return CallOutcome::OverRequestLimit(text);
        }
        if status == 402 || status == 429 {
            return CallOutcome::ServiceLimit(format!("classifier.dev returned {status}: {text}"));
        }
        if status != 200 {
            return CallOutcome::Failed(eyre!("classifier.dev returned {status}: {text}"));
        }
        let parsed: ClassifyResponse = match serde_json::from_str(&text) {
            Ok(parsed) => parsed,
            Err(e) => {
                return CallOutcome::Failed(eyre!("failed to parse classifier.dev response: {e}: {text}"));
            }
        };
        if let Err(e) = ensure_jev(&parsed) {
            return CallOutcome::Failed(e);
        }
        let usage = parsed.call_usage();
        log::debug!(
            "ClassifierDev::post: texts={} labels={} input_tokens={} estimated_usd={}",
            texts.len(),
            labels.len(),
            usage.input_tokens,
            usage.estimated_usd
        );
        CallOutcome::Scored(parsed)
    }

    /// Every vocabulary slug's mean score (over `repeats` calls) for each
    /// input: `repeats` calls per (chunk x label shard), with the result
    /// decided per note.
    ///
    /// Two API ceilings shape the calls: at most `MAX_LABELS_PER_CALL` labels
    /// (the vocabulary is 117, so production always shards) and the free
    /// tier's per-request spend, which `plan_chunks` stays under and a 402
    /// `request_spending_limit` enforces by halving the chunk down to one
    /// note. A note is `Ok` only if every repeat of every shard scored every
    /// label it sent: a note that fails on any repeat of a shard is that
    /// note's `Err` for the whole batch, never `Ok` with the average of
    /// whichever repeats happened to succeed, and it is not sent to the next
    /// shard (or the shard's remaining repeats, once pending drops to none).
    pub fn score_batch(&self, inputs: &[TagInput]) -> ScoredBatch {
        let texts: Vec<String> = inputs.iter().map(|i| clip_classifier_text(i.title, i.text)).collect();
        let label_shards = self.label_shards();
        log::debug!(
            "ClassifierDev::score_batch: texts={} shards={} repeats={} threshold={}",
            texts.len(),
            label_shards.len(),
            self.repeats,
            self.threshold
        );

        let mut per_note: Vec<Result<Vec<HashMap<String, f32>>>> = inputs.iter().map(|_| Ok(Vec::new())).collect();
        let mut calls = Vec::new();
        let mut service_limit = None;
        for pairs in &label_shards {
            let wire_labels: Vec<String> = pairs.iter().map(|(wire, _)| wire.clone()).collect();
            let wire_to_slug: HashMap<String, String> = pairs.iter().cloned().collect();

            // Sum and count of each label's score across this shard's
            // repeats, for the notes still pending going into it. `Err`
            // means some repeat of this shard already failed the note; it is
            // folded into `per_note` only once every repeat has run (or the
            // shard gives up early), never from a partial average.
            let mut shard_acc: Vec<Result<HashMap<String, (f32, u32)>>> = per_note
                .iter()
                .map(|note| match note {
                    Ok(_) => Ok(HashMap::new()),
                    Err(_) => Err(eyre!("note already failed a prior shard")),
                })
                .collect();

            for repeat in 0..self.repeats {
                let pending: Vec<usize> = (0..inputs.len()).filter(|i| shard_acc[*i].is_ok()).collect();
                if pending.is_empty() {
                    break;
                }
                log::debug!(
                    "ClassifierDev::score_batch: shard repeat {}/{} over {} notes",
                    repeat + 1,
                    self.repeats,
                    pending.len()
                );
                let lengths: Vec<usize> = pending.iter().map(|i| texts[*i].chars().count()).collect();
                let mut start = 0usize;
                for size in plan_chunks(&lengths, &wire_labels) {
                    let chunk = &pending[start..start + size];
                    self.score_chunk(
                        &wire_labels,
                        &wire_to_slug,
                        chunk,
                        &texts,
                        &mut shard_acc,
                        &mut calls,
                        &mut service_limit,
                    );
                    start += size;
                }
            }

            for (i, note) in per_note.iter_mut().enumerate() {
                if note.is_err() {
                    continue;
                }
                match &shard_acc[i] {
                    Ok(sums) => {
                        let averaged: HashMap<String, f32> = sums
                            .iter()
                            .map(|(slug, (sum, count))| (slug.clone(), sum / *count as f32))
                            .collect();
                        if let Ok(shard_scores) = note {
                            shard_scores.push(averaged);
                        }
                    }
                    Err(e) => *note = Err(eyre!("{e:#}")),
                }
            }
        }

        let notes = per_note
            .into_iter()
            .map(|note| note.map(|shard_scores| merge_shard_scores(&shard_scores).into_iter().collect()))
            .collect();
        ScoredBatch {
            notes,
            calls,
            service_limit,
        }
    }

    /// Our estimate of each input's cost across every shard, from the same
    /// wire text and token model `score_batch` chunks by.
    pub fn estimate_batch(&self, inputs: &[TagInput]) -> SpendEstimate {
        let texts: Vec<String> = inputs.iter().map(|i| clip_classifier_text(i.title, i.text)).collect();
        SpendEstimate {
            per_note_usd: self.wire_usd(&texts),
        }
    }

    /// Every shard's wire labels only, in `label_shards`' order. The token
    /// model prices whatever text is actually sent, so a described label
    /// (longer than its bare slug) costs more here too.
    fn wire_label_shards(&self) -> Vec<Vec<String>> {
        self.label_shards()
            .into_iter()
            .map(|pairs| pairs.into_iter().map(|(wire, _)| wire).collect())
            .collect()
    }

    /// Priced at `repeats` calls per shard: `score_batch` posts each shard
    /// `repeats` times, so the estimate a bulk caller checks against its
    /// allowance (`BulkRun::run`, `estimate_batch`) must scale the same way.
    fn wire_usd(&self, texts: &[String]) -> Vec<f64> {
        let shards = self.wire_label_shards();
        let repeats = f64::from(self.repeats);
        texts
            .iter()
            .map(|text| {
                let chars = text.chars().count();
                let tokens: f64 = shards.iter().map(|shard| note_tokens(chars, shard)).sum::<f64>() * repeats;
                tokens * USD_PER_INPUT_TOKEN
            })
            .collect()
    }

    /// Consecutive chunk sizes over `texts` such that each chunk fits one
    /// request on every shard: planned against the shard whose labels cost
    /// the most per note, which bounds the others. This is one physical
    /// request's budget; `repeats` multiplies how many such requests
    /// `score_batch` sends per chunk, not this chunk's own size.
    fn plan_run_chunks(&self, texts: &[String]) -> Vec<usize> {
        let shards = self.wire_label_shards();
        let Some(widest) = shards
            .iter()
            .max_by(|a, b| note_tokens(0, a).total_cmp(&note_tokens(0, b)))
        else {
            return Vec::new();
        };
        let lengths: Vec<usize> = texts.iter().map(|t| t.chars().count()).collect();
        plan_chunks(&lengths, widest)
    }

    /// A stable digest of everything that changes a score: the endpoint, the
    /// labels exactly as sent (shard by shard, descriptions included),
    /// `max_labels`, `instructions`, and `repeats` (more or fewer repeats
    /// changes the averaged score), and the wire text bound. A stored score
    /// is reusable only under the same digest.
    pub fn scoring_config_hash(&self) -> String {
        let shards = self.wire_label_shards();
        let config = serde_json::json!({
            "endpoint": self.endpoint,
            "shards": shards,
            "max_labels": self.canon.max_per_note,
            "text_chars": CLASSIFIER_TEXT_CHARS,
            "instructions": self.instructions,
            "repeats": self.repeats,
        });
        sha256_hex(config.to_string().as_bytes())
    }

    /// Score the notes at `chunk` (indices into `texts`) against one call of
    /// one shard, halving on a per-request spend refusal. Each call appends
    /// one usage row and adds its scores into `acc`'s running sum/count per
    /// slug, so the repeat this call belongs to averages with its siblings
    /// rather than overwriting them.
    ///
    /// `labels` is the shard's wire labels (post-description); `wire_to_slug`
    /// maps each back to the slug it describes, so a returned score lands on
    /// `homelab`, not on `"homelab: self-hosted servers..."`.
    fn score_chunk(
        &self,
        labels: &[String],
        wire_to_slug: &HashMap<String, String>,
        chunk: &[usize],
        texts: &[String],
        acc: &mut [Result<HashMap<String, (f32, u32)>>],
        calls: &mut Vec<CallUsage>,
        service_limit: &mut Option<String>,
    ) {
        if let Some(limit) = service_limit {
            for i in chunk {
                acc[*i] = Err(eyre!("not sent: {limit}"));
            }
            return;
        }
        let chunk_texts: Vec<String> = chunk.iter().map(|i| texts[*i].clone()).collect();
        match self.post(labels, &chunk_texts) {
            CallOutcome::Scored(response) => {
                calls.push(response.call_usage());
                if response.results.len() != chunk.len() {
                    let got = response.results.len();
                    for i in chunk {
                        acc[*i] = Err(eyre!(
                            "classifier.dev returned {got} results for {} inputs",
                            chunk.len()
                        ));
                    }
                    return;
                }
                for (i, result) in chunk.iter().zip(response.results) {
                    match check_shard_scores(labels, result.scores) {
                        Ok(scores) => {
                            if let Ok(sums) = &mut acc[*i] {
                                for (wire, score) in scores {
                                    let slug = wire_to_slug
                                        .get(&wire)
                                        .cloned()
                                        .expect("check_shard_scores validated this key against the sent labels");
                                    let entry = sums.entry(slug).or_insert((0.0, 0));
                                    entry.0 += score;
                                    entry.1 += 1;
                                }
                            }
                        }
                        Err(e) => acc[*i] = Err(e),
                    }
                }
            }
            CallOutcome::OverRequestLimit(body) => {
                calls.push(CallUsage::failed());
                if chunk.len() > 1 {
                    log::debug!(
                        "ClassifierDev::score_chunk: 402 request_spending_limit on {} notes; splitting",
                        chunk.len()
                    );
                    let (left, right) = chunk.split_at(chunk.len() / 2);
                    self.score_chunk(labels, wire_to_slug, left, texts, acc, calls, service_limit);
                    self.score_chunk(labels, wire_to_slug, right, texts, acc, calls, service_limit);
                } else {
                    acc[chunk[0]] = Err(eyre!(
                        "classifier.dev refused this note alone over its per-request spend limit: {body}"
                    ));
                }
            }
            CallOutcome::ServiceLimit(limit) => {
                log::warn!("ClassifierDev::score_chunk: {limit}; sending nothing more in this batch");
                calls.push(CallUsage::failed());
                for i in chunk {
                    acc[*i] = Err(eyre!("{limit}"));
                }
                *service_limit = Some(limit);
            }
            CallOutcome::Failed(e) => {
                calls.push(CallUsage::failed());
                for i in chunk {
                    acc[*i] = Err(eyre!("{e:#}"));
                }
            }
        }
    }

    /// Select a note's tags from its complete score map.
    fn finish(&self, input: &TagInput, scores: &HashMap<String, f32>) -> TagOutput {
        let scored: Vec<(String, f32)> = scores.iter().map(|(t, s)| (t.clone(), *s)).collect();
        let mut selected = select_above_threshold(&scored, self.threshold, self.canon.max_per_note);

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
        self.classify_batch(std::slice::from_ref(input))
            .pop()
            .ok_or_else(|| eyre!("classifier.dev returned no result for a single input"))?
    }

    /// Per note: one note's failure is that note's `Err` (and its caller's
    /// fallback), not the whole batch's.
    fn classify_batch(&self, inputs: &[TagInput]) -> Vec<Result<TagOutput>> {
        self.classify_batch_metered(inputs).outputs
    }

    fn classify_batch_metered(&self, inputs: &[TagInput]) -> MeteredBatch {
        let batch = self.score_batch(inputs);
        let outputs = inputs
            .iter()
            .zip(batch.notes)
            .map(|(input, scores)| scores.map(|s| self.finish(input, &s)))
            .collect();
        MeteredBatch {
            outputs,
            calls: batch.calls,
            service_limit: batch.service_limit,
        }
    }

    fn estimate(&self, inputs: &[TagInput]) -> Option<SpendEstimate> {
        Some(self.estimate_batch(inputs))
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
