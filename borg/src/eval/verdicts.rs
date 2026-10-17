//! `sb borg eval --ingest-verdicts`: load the `eval/` tools' verdict JSONL into
//! `eval_scores`.
//!
//! The contract (design: `2026-10-14-automated-note-quality-loop.md`, the `uv`
//! project): each verdict copies `trace`, `run_kind`, `run_id`,
//! `rubric_version` and `content_hash` from the record it judged, and carries
//! `tool`, `tool_version`, `model`, an optional `claim_id`, a `metric`, a
//! `value` and an optional `detail`. The full `eval_scores` key is rebuilt from
//! those fields with `check_name = tool` and `judge_model = model`.
//!
//! One tool emits many verdicts per key (a claim each, or several metrics), so
//! verdicts sharing a key fold into one row: note-level metrics under
//! `metrics`, per-claim ones under `claims.<claim_id>`.
//!
//! A verdict loads only while its `content_hash` still describes the run it
//! names: a baseline against the trace's current distill hash in staging, any
//! other run against a row that run already has at that hash. Anything else is
//! stale, is dropped, and is named.

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use super::scores::{ScoreRow, ScoreStore, rubric_version_of};
use super::{RUN_KIND_BASELINE, judged_input, load_staged_trace};

/// One verdict line. Unknown keys are an error: a tool that grows a field `sb`
/// does not know has drifted from the contract and must say so loudly.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub trace: String,
    pub run_kind: String,
    pub run_id: String,
    pub rubric_version: String,
    pub content_hash: String,
    pub tool: String,
    pub tool_version: String,
    pub model: String,
    #[serde(default)]
    pub claim_id: Option<Value>,
    pub metric: String,
    pub value: f64,
    #[serde(default)]
    pub detail: Option<Value>,
}

/// The run a verdict is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RunKey {
    trace: String,
    run_kind: String,
    run_id: String,
    content_hash: String,
}

impl RunKey {
    fn of(v: &Verdict) -> Self {
        Self {
            trace: v.trace.clone(),
            run_kind: v.run_kind.clone(),
            run_id: v.run_id.clone(),
            content_hash: v.content_hash.clone(),
        }
    }
}

/// A trace's current baseline identity in staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentBaseline {
    pub content_hash: String,
    pub kind: String,
}

/// The current baseline of a trace in `staged_root`, or `None` when it is no
/// longer staged. Uses the same input and hash the scorer uses.
pub fn staged_baseline(staged_root: &Path, trace: &str) -> Result<Option<CurrentBaseline>> {
    Ok(load_staged_trace(staged_root, trace)?.map(|fixture| CurrentBaseline {
        content_hash: judged_input(&fixture).content_hash,
        kind: fixture.kind,
    }))
}

/// A dropped verdict group and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub trace: String,
    pub run_kind: String,
    pub run_id: String,
    pub content_hash: String,
    pub verdicts: usize,
    pub reason: String,
}

/// What one `--ingest-verdicts` file did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestOutcome {
    pub verdicts_read: usize,
    pub verdicts_loaded: usize,
    pub rows_written: usize,
    pub rejected: Vec<Rejection>,
}

impl IngestOutcome {
    /// One line per rejection, each naming its trace.
    pub fn rejection_lines(&self) -> Vec<String> {
        self.rejected
            .iter()
            .map(|r| {
                format!(
                    "rejected {} verdict(s) for trace {} ({} run '{}', content_hash {}): {}",
                    r.verdicts, r.trace, r.run_kind, r.run_id, r.content_hash, r.reason
                )
            })
            .collect()
    }

    /// The failing exit: any rejected verdict is an error naming every trace,
    /// after the valid ones have loaded.
    pub fn check(&self) -> Result<()> {
        if self.rejected.is_empty() {
            return Ok(());
        }
        let mut traces: Vec<&str> = self.rejected.iter().map(|r| r.trace.as_str()).collect();
        traces.sort_unstable();
        traces.dedup();
        eyre::bail!(
            "{} verdict group(s) rejected as stale for trace(s): {}",
            self.rejected.len(),
            traces.join(", ")
        )
    }
}

/// Parse a verdict file. A malformed line is an error naming its line.
pub fn read_verdicts(path: &Path) -> Result<Vec<Verdict>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading verdict file {}", path.display()))?;
    let mut verdicts = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let number = index + 1;
        let verdict: Verdict =
            serde_json::from_str(line).with_context(|| format!("{}:{number}: not a verdict record", path.display()))?;
        validate(&verdict).with_context(|| format!("{}:{number}", path.display()))?;
        verdicts.push(verdict);
    }
    Ok(verdicts)
}

fn validate(v: &Verdict) -> Result<()> {
    for (name, value) in [
        ("trace", &v.trace),
        ("run_kind", &v.run_kind),
        ("rubric_version", &v.rubric_version),
        ("content_hash", &v.content_hash),
        ("tool", &v.tool),
        ("tool_version", &v.tool_version),
        ("metric", &v.metric),
    ] {
        if value.trim().is_empty() {
            eyre::bail!("verdict has an empty {name}");
        }
    }
    if v.model.trim().is_empty() {
        eyre::bail!("verdict has an empty model: '' is reserved for deterministic checks");
    }
    if rubric_version_of(&v.tool).is_some() {
        eyre::bail!(
            "verdict tool '{}' is a built-in check name; a tool may not write that check's rows",
            v.tool
        );
    }
    if !v.value.is_finite() {
        eyre::bail!("verdict value for {} {} is not finite", v.trace, v.metric);
    }
    Ok(())
}

/// Why `key` is stale, or the `kind` its rows carry when it is current.
fn judge_run(
    key: &RunKey,
    store: &ScoreStore,
    baseline: &dyn Fn(&str) -> Result<Option<CurrentBaseline>>,
) -> Result<std::result::Result<String, String>> {
    if key.run_kind == RUN_KIND_BASELINE {
        if !key.run_id.is_empty() {
            return Ok(Err(format!("a baseline run has no run_id, got '{}'", key.run_id)));
        }
        return Ok(match baseline(&key.trace)? {
            None => Err("the trace is no longer in staging, so its content_hash cannot be confirmed".to_string()),
            Some(current) if current.content_hash == key.content_hash => Ok(current.kind),
            Some(current) => Err(format!(
                "stale content_hash: the trace's current distill hash is {}",
                current.content_hash
            )),
        });
    }
    Ok(
        match store.run_kind_of(&key.trace, &key.run_kind, &key.run_id, &key.content_hash)? {
            Some(kind) => Ok(kind),
            None => Err(format!(
                "no {} run '{}' scored at this content_hash",
                key.run_kind, key.run_id
            )),
        },
    )
}

/// Fold verdicts that share one `eval_scores` key into the row's metrics.
fn metrics_of(group: &[&Verdict]) -> Result<Value> {
    let first = group[0];
    let mut metrics = serde_json::Map::new();
    let mut claims: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    for v in group {
        if v.tool_version != first.tool_version {
            eyre::bail!(
                "{} verdicts for {} carry two tool versions: {} and {}",
                v.tool,
                v.trace,
                first.tool_version,
                v.tool_version
            );
        }
        let mut entry = serde_json::Map::new();
        entry.insert("value".into(), json!(v.value));
        if let Some(detail) = &v.detail {
            entry.insert("detail".into(), detail.clone());
        }
        let slot = match &v.claim_id {
            None => &mut metrics,
            Some(id) => {
                let id = match id {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                claims.entry(id).or_default()
            }
        };
        if slot.insert(v.metric.clone(), Value::Object(entry)).is_some() {
            eyre::bail!(
                "{} wrote metric '{}' twice for {} (claim {:?})",
                v.tool,
                v.metric,
                v.trace,
                v.claim_id
            );
        }
    }
    let mut out = json!({ "tool_version": first.tool_version, "metrics": metrics });
    if !claims.is_empty() {
        out["claims"] = json!(claims);
    }
    Ok(out)
}

/// Load `verdicts` into `store`. `baseline` answers "what is this trace's
/// current distill hash" for baseline verdicts (production: staging).
pub fn ingest(
    verdicts: &[Verdict],
    store: &ScoreStore,
    baseline: &dyn Fn(&str) -> Result<Option<CurrentBaseline>>,
    scored_at: &str,
) -> Result<IngestOutcome> {
    log::debug!("eval::verdicts::ingest: verdicts={}", verdicts.len());
    let mut outcome = IngestOutcome {
        verdicts_read: verdicts.len(),
        ..IngestOutcome::default()
    };

    let mut by_run: BTreeMap<RunKey, Vec<&Verdict>> = BTreeMap::new();
    for v in verdicts {
        by_run.entry(RunKey::of(v)).or_default().push(v);
    }

    let mut rows = Vec::new();
    for (key, run_verdicts) in by_run {
        let kind = match judge_run(&key, store, baseline)? {
            Ok(kind) => kind,
            Err(reason) => {
                outcome.rejected.push(Rejection {
                    trace: key.trace,
                    run_kind: key.run_kind,
                    run_id: key.run_id,
                    content_hash: key.content_hash,
                    verdicts: run_verdicts.len(),
                    reason,
                });
                continue;
            }
        };
        let mut by_row: BTreeMap<(&str, &str, &str), Vec<&Verdict>> = BTreeMap::new();
        for v in &run_verdicts {
            by_row
                .entry((v.tool.as_str(), v.model.as_str(), v.rubric_version.as_str()))
                .or_default()
                .push(v);
        }
        for ((tool, model, rubric), group) in by_row {
            rows.push(ScoreRow {
                trace: key.trace.clone(),
                run_kind: key.run_kind.clone(),
                run_id: key.run_id.clone(),
                kind: kind.clone(),
                judge_model: model.to_string(),
                check_name: tool.to_string(),
                rubric_version: rubric.to_string(),
                content_hash: key.content_hash.clone(),
                scored_at: scored_at.to_string(),
                metrics_json: metrics_of(&group)?.to_string(),
                error: None,
            });
            outcome.verdicts_loaded += group.len();
        }
    }
    // Every row is built before the first write, so a malformed group fails the
    // whole file rather than leaving half of it loaded.
    for row in &rows {
        store.put(row)?;
    }
    outcome.rows_written = rows.len();
    Ok(outcome)
}

#[cfg(test)]
mod tests;
