//! The one clustering call: failing traces in, failure categories out.
//!
//! The model names categories and assigns traces to them; everything else is
//! Rust. Counts are the length of the validated trace list, never a number the
//! model wrote, a trace id the model invented is dropped (and counted), and a
//! failing trace the model left out lands in an `Unclustered` category, so no
//! failure vanishes from the report.

use std::collections::BTreeSet;

use eyre::{Context, Result};
use serde::Deserialize;
use vault::fabric::ApiKeys;

/// The category that collects failing traces the model assigned nowhere.
pub const UNCLUSTERED: &str = "Unclustered";
/// Examples shown per category.
pub const EXAMPLES_PER_CATEGORY: usize = 3;
/// Head budget (chars) of the clustering input.
const MAX_INPUT_CHARS: usize = 120_000;
const CLUSTER_TIMEOUT_SECS: u64 = 300;
/// Longest claim text handed to the model, per claim.
const CLAIM_TEXT_CHARS: usize = 160;

/// One failing trace as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFailures {
    pub trace: String,
    /// The trace's note kind (`article`, `video`, ...).
    pub kind: String,
    /// One line per failing check, plus the claim texts a judge rejected.
    pub entries: Vec<String>,
}

/// A category after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub name: String,
    pub description: String,
    /// Distinct failing traces, in input order.
    pub traces: Vec<String>,
}

impl Cluster {
    pub fn examples(&self) -> &[String] {
        &self.traces[..self.traces.len().min(EXAMPLES_PER_CATEGORY)]
    }
}

/// What came back from the model, validated against what went in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Clustering {
    /// Largest first, ties by name; `Unclustered` sorts with the rest.
    pub clusters: Vec<Cluster>,
    /// Trace ids the model named that were not in the input.
    pub unknown_ids: usize,
}

/// Runs the model: the prompt input in, the raw reply out.
pub trait Clusterer {
    fn cluster(&self, input: &str) -> Result<String>;
}

/// Production [`Clusterer`]: one fabric call.
pub struct FabricClusterer {
    pub binary: String,
    /// Fabric `-m`; empty is fabric's own default model.
    pub model: String,
    pub api_keys: ApiKeys,
    /// The pattern argument for fabric (a name, or an absolute path under
    /// `--patterns-dir`).
    pub pattern: String,
}

impl Clusterer for FabricClusterer {
    fn cluster(&self, input: &str) -> Result<String> {
        log::debug!(
            "eval::weekly::cluster::FabricClusterer::cluster: pattern={} model={} input_len={}",
            self.pattern,
            self.model,
            input.len()
        );
        let command = vault::fabric::command(&self.binary, &self.pattern, &self.model, &self.api_keys, 0);
        vault::fabric::run_command(command, &self.pattern, input, MAX_INPUT_CHARS, CLUSTER_TIMEOUT_SECS)
            .context("the weekly failure-clustering call failed")
    }
}

/// The text the model reads: one block per failing trace.
pub fn prompt_input(traces: &[TraceFailures]) -> String {
    let mut out = String::new();
    for t in traces {
        out.push_str(&format!("trace: {}\nkind: {}\n", t.trace, t.kind));
        for entry in &t.entries {
            out.push_str(&format!("- {entry}\n"));
        }
        out.push('\n');
    }
    out
}

/// A claim text trimmed for the prompt, on a char boundary.
pub fn claim_excerpt(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= CLAIM_TEXT_CHARS {
        return one_line;
    }
    let cut: String = one_line.chars().take(CLAIM_TEXT_CHARS).collect();
    format!("{cut}...")
}

#[derive(Debug, Deserialize)]
struct Reply {
    categories: Vec<RawCategory>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCategory {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub traces: Vec<String>,
}

/// Parse the model's reply. Lenient about a code fence or a prose preamble
/// (the span from the first `{` to the last `}` is the object), strict about
/// the shape: anything else is an error, never an empty clustering.
pub fn parse_reply(reply: &str) -> Result<Vec<RawCategory>> {
    let (Some(start), Some(end)) = (reply.find('{'), reply.rfind('}')) else {
        eyre::bail!(
            "the clustering reply holds no JSON object: {:?}",
            reply.chars().take(120).collect::<String>()
        );
    };
    let parsed: Reply = serde_json::from_str(&reply[start..=end]).with_context(|| {
        format!(
            "the clustering reply is not the expected {{\"categories\":[...]}} object: {:?}",
            reply.chars().take(120).collect::<String>()
        )
    })?;
    Ok(parsed.categories)
}

/// Validate `raw` against `input`: drop unknown trace ids (counted), drop
/// categories left empty, de-duplicate within a category, and gather every
/// unassigned failing trace under [`UNCLUSTERED`].
pub fn assemble(raw: Vec<RawCategory>, input: &[TraceFailures]) -> Clustering {
    let known: Vec<&str> = input.iter().map(|t| t.trace.as_str()).collect();
    let known_set: BTreeSet<&str> = known.iter().copied().collect();
    let mut unknown_ids = 0;
    let mut assigned: BTreeSet<&str> = BTreeSet::new();
    let mut clusters = Vec::new();
    for category in &raw {
        let named: BTreeSet<&str> = category.traces.iter().map(String::as_str).collect();
        unknown_ids += named.iter().filter(|t| !known_set.contains(**t)).count();
        let traces: Vec<&str> = known.iter().copied().filter(|t| named.contains(t)).collect();
        if traces.is_empty() {
            continue;
        }
        assigned.extend(traces.iter().copied());
        clusters.push(Cluster {
            name: category.name.trim().to_string(),
            description: category.description.trim().to_string(),
            traces: traces.into_iter().map(str::to_string).collect(),
        });
    }
    let left: Vec<String> = known
        .iter()
        .filter(|t| !assigned.contains(**t))
        .map(|t| (*t).to_string())
        .collect();
    if !left.is_empty() {
        clusters.push(Cluster {
            name: UNCLUSTERED.to_string(),
            description: "Failing traces the model assigned to no category.".to_string(),
            traces: left,
        });
    }
    clusters.sort_by(|a, b| b.traces.len().cmp(&a.traces.len()).then_with(|| a.name.cmp(&b.name)));
    Clustering { clusters, unknown_ids }
}

/// Cluster `input` with one `clusterer` call. No failing trace, no call.
pub fn cluster_failures(clusterer: &dyn Clusterer, input: &[TraceFailures]) -> Result<Clustering> {
    log::debug!("eval::weekly::cluster::cluster_failures: traces={}", input.len());
    if input.is_empty() {
        return Ok(Clustering::default());
    }
    let reply = clusterer.cluster(&prompt_input(input))?;
    Ok(assemble(parse_reply(&reply)?, input))
}
