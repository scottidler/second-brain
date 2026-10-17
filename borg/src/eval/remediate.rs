//! `sb borg eval --remediate [--apply]`: one automatic re-distill per failing
//! source, never a loop.
//!
//! Design: `docs/design/2026-10-14-automated-note-quality-loop.md`
//! (Architecture, Remediation; "What an untrusted judge is barred from").
//!
//! - **Failure:** a trace's current, error-free `baseline` row for a required
//!   check ([`super::scores`]) that fails ([`row_failure`]). Error rows and
//!   rows from checks outside the required set never trigger anything.
//! - **Gate:** a deterministic check's failure always acts; a judge check's
//!   acts only when [`trust::judge_check_trust`] says trusted. A failure only
//!   an untrusted judge reports is listed with the judge, check and each
//!   planted class that failed the gate, and nothing is posted.
//! - **One attempt per source URL:** the claim ([`claim`]) counts the prior
//!   attempts for the URL (claims in `remediations` plus receipts rows with
//!   `replay_origin = 'auto-remediation'`) and inserts its own claim only when
//!   that count is 0, in one `BEGIN IMMEDIATE` transaction on the receipts
//!   DB, so two concurrent runs cannot both post. A remediated trace that
//!   fails again finds its source's count at 1.
//! - **Lineage:** the POST carries `replay-of` (the failing trace) and
//!   `replay-origin: auto-remediation`; the daemon writes both in the INSERT
//!   that creates the new receipts row. `/health` must advertise
//!   [`FEATURE_REPLAY_OF`] first: an older daemon drops the fields silently.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use vault::daemon::client::{DaemonClient, DaemonError};

use super::agreement::trace_flag;
use super::scores::{
    CHECK_LISTICLE_SURVIVAL, CHECK_NOTE_SIZE, CHECK_PLACEHOLDER_ARTIFACTS, CHECK_QUOTE_GROUNDING,
    CHECK_TOOLS_GROUNDING, RequiredCheck, ScoreRow, ScoreStore, required_checks,
};
use super::trust::{self, TrustRow, TrustState};
use super::{CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, judged_input, load_staged_trace};
use crate::config::TrustConfig;
use crate::health::{FEATURE_REPLAY_OF, HealthResponse};
use crate::types::{IngestResult, IngestStatus, ReplayOrigin};

/// The receipts `kind` of the only source a remediation can re-post: a URL.
const RECEIPT_KIND_URL: &str = "url";

/// One failing current row and whether it may act.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// `""` for a deterministic check.
    pub judge_model: String,
    pub check: String,
    pub rubric_version: String,
    /// The trace's distill hash the row was scored at (its current hash).
    pub content_hash: String,
    /// The row's `metrics_json`, frozen onto the claim when this failure
    /// triggers a remediation.
    pub metrics_json: String,
    /// What failed, in words.
    pub reason: String,
    /// `Ungated` (deterministic), `Trusted`, or `Untrusted` with each
    /// planted class that failed the gate.
    pub gate: TrustState,
}

/// A trace with at least one failing current row.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub trace: String,
    /// Receipts `kind` (`url`, `text`, `binary`, `session`).
    pub kind: String,
    /// Receipts `method`, the original transport; the re-POST carries it so the
    /// note's `method:` stays true.
    pub method: String,
    /// Receipts `raw_input`: the source URL for a `url` receipt.
    pub source: String,
    pub failures: Vec<Failure>,
}

impl Candidate {
    /// The failure that justifies the remediation: the first one that may act.
    /// `None` when every failure comes from an untrusted judge check.
    pub fn trigger(&self) -> Option<&Failure> {
        self.failures.iter().find(|f| f.gate.may_act())
    }
}

/// Every failing scored trace, and what the scan could not read.
#[derive(Debug, Default)]
pub struct Plan {
    /// Traces with an error-free baseline row that the scan looked at.
    pub scored: usize,
    /// Scored traces swept from staging: their current hash is unknown, so no
    /// row of theirs is current.
    pub not_staged: usize,
    /// Scored traces with no `succeeded` receipt on this host.
    pub no_receipt: usize,
    /// Staged traces that do not parse, with why.
    pub unreadable: Vec<(String, String)>,
    /// Failing traces, by trace id.
    pub candidates: Vec<Candidate>,
}

/// Why one row fails, or `None` when it passes. Judge checks use the same
/// flag rule as the agreement report and `--inject` (`agreement::trace_flag`,
/// `inject::flags`); the deterministic checks fail as the text report reads
/// them. A row whose metrics cannot be read is an error, never a pass.
pub fn row_failure(row: &ScoreRow) -> Result<Option<String>> {
    let metrics: Value = serde_json::from_str(&row.metrics_json)
        .with_context(|| format!("trace {} check {}: unreadable metrics_json", row.trace, row.check_name))?;
    if metrics.get("applicable") == Some(&Value::Bool(false)) {
        return Ok(None);
    }
    let unreadable = || {
        eyre::eyre!(
            "trace {} check {}: metrics_json {} does not carry the check's fields",
            row.trace,
            row.check_name,
            row.metrics_json
        )
    };
    let count = |name: &str| metrics.get(name).and_then(Value::as_u64).ok_or_else(unreadable);
    let reason = match row.check_name.as_str() {
        CHECK_JUDGE_DISTILLATION => trace_flag(row).ok_or_else(unreadable)?.then(|| {
            format!(
                "axes coverage {} / anchors {} / faithfulness {} (3 is a pass)",
                metrics["claim_coverage"], metrics["anchor_validity"], metrics["summary_faithfulness"]
            )
        }),
        CHECK_JUDGE_CLAIMS => {
            let verdicts = metrics
                .get("verdicts")
                .and_then(Value::as_array)
                .ok_or_else(unreadable)?;
            trace_flag(row).ok_or_else(unreadable)?.then(|| {
                let unsupported = verdicts.iter().filter(|v| v.as_str() != Some("supported")).count();
                format!("{unsupported} of {} claims not supported", verdicts.len())
            })
        }
        CHECK_QUOTE_GROUNDING => {
            let (grounded, quoted) = (count("grounded")?, count("quoted")?);
            (grounded < quoted).then(|| format!("{grounded} of {quoted} quotes grounded in the source"))
        }
        CHECK_TOOLS_GROUNDING => {
            let (names_grounded, names) = (count("names_grounded")?, count("names")?);
            let (commands_grounded, commands) = (count("commands_grounded")?, count("commands")?);
            (names_grounded < names || commands_grounded < commands).then(|| {
                format!("{names_grounded} of {names} tool names, {commands_grounded} of {commands} commands grounded")
            })
        }
        CHECK_PLACEHOLDER_ARTIFACTS => {
            let reasons = metrics
                .get("reasons")
                .and_then(Value::as_array)
                .ok_or_else(unreadable)?;
            (!reasons.is_empty()).then(|| {
                let joined: Vec<&str> = reasons.iter().filter_map(Value::as_str).collect();
                format!("placeholder filler: {}", joined.join("; "))
            })
        }
        CHECK_LISTICLE_SURVIVAL => {
            let score = metrics.get("score").and_then(Value::as_f64).ok_or_else(unreadable)?;
            (score < 1.0).then(|| format!("listicle survival {score:.2} < 1.00"))
        }
        CHECK_NOTE_SIZE => {
            let within = metrics
                .get("within_ceiling")
                .and_then(Value::as_bool)
                .ok_or_else(unreadable)?;
            (!within).then(|| format!("rendered {} bytes, over the ceiling", metrics["rendered_bytes"]))
        }
        other => eyre::bail!("trace {}: no failure rule for check '{other}'", row.trace),
    };
    Ok(reason)
}

/// Scan every scored trace for failing current rows. `judge_models` is
/// `eval.judge-models` (the required set's judge half); `trust_rows` and
/// `policy` decide each judge check's gate.
pub fn plan(
    receipts: &Connection,
    store: &ScoreStore,
    staged_root: &Path,
    judge_models: &[String],
    trust_rows: &[TrustRow],
    policy: &TrustConfig,
) -> Result<Plan> {
    log::debug!(
        "eval::remediate::plan: root={} judge_models={judge_models:?} trust_rows={}",
        staged_root.display(),
        trust_rows.len()
    );
    let required: BTreeSet<RequiredCheck> = required_checks(judge_models).into_iter().collect();
    let mut traces = store.latest_scored_traces()?;
    traces.sort();
    let mut out = Plan {
        scored: traces.len(),
        ..Plan::default()
    };
    for trace in traces {
        let Some(receipt) = crate::receipts::get(receipts, &trace)?.filter(|r| r.status == "succeeded") else {
            out.no_receipt += 1;
            continue;
        };
        let fixture = match load_staged_trace(staged_root, &trace) {
            Ok(Some(fixture)) => fixture,
            Ok(None) => {
                out.not_staged += 1;
                continue;
            }
            Err(e) => {
                out.unreadable.push((trace, format!("{e:#}")));
                continue;
            }
        };
        let hash = judged_input(&fixture).content_hash;
        let mut failures = Vec::new();
        for row in store.baseline_rows_at(&trace, &hash)? {
            let key = RequiredCheck {
                judge_model: row.judge_model.clone(),
                check_name: row.check_name.clone(),
                rubric_version: row.rubric_version.clone(),
            };
            if row.error.is_some() || !required.contains(&key) {
                continue;
            }
            let Some(reason) = row_failure(&row)? else {
                continue;
            };
            failures.push(Failure {
                gate: trust::judge_check_trust(&row.judge_model, &row.check_name, trust_rows, policy),
                judge_model: row.judge_model,
                check: row.check_name,
                rubric_version: row.rubric_version,
                content_hash: row.content_hash,
                metrics_json: row.metrics_json,
                reason,
            });
        }
        if !failures.is_empty() {
            // Deterministic failures first, so the trigger is the ungated one.
            failures.sort_by(|a, b| (!a.judge_model.is_empty(), &a.check).cmp(&(!b.judge_model.is_empty(), &b.check)));
            out.candidates.push(Candidate {
                trace: receipt.trace_id,
                kind: receipt.kind,
                method: receipt.method,
                source: receipt.raw_input,
                failures,
            });
        }
    }
    log::debug!(
        "eval::remediate::plan: scored={} failing={} not_staged={} no_receipt={}",
        out.scored,
        out.candidates.len(),
        out.not_staged,
        out.no_receipt
    );
    Ok(out)
}

/// Prior auto-remediation attempts for `source_url`: claims, plus intake rows
/// carrying `replay_origin = 'auto-remediation'` that no claim accounts for
/// (an attempt counts once its intake row exists, whatever happened after).
pub fn prior_attempts(conn: &Connection, source_url: &str) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM remediations WHERE source_url = ?1)
                  + (SELECT COUNT(*) FROM receipts
                     WHERE raw_input = ?1 AND replay_origin = ?2
                       AND trace_id NOT IN (SELECT remediation_trace FROM remediations
                                            WHERE source_url = ?1 AND remediation_trace IS NOT NULL))",
            params![source_url, ReplayOrigin::AutoRemediation.as_str()],
            |r| r.get(0),
        )
        .with_context(|| format!("counting remediation attempts for {source_url}"))?;
    Ok(n as usize)
}

/// The claim's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// This run owns the source's one attempt and must post it.
    Claimed,
    /// An earlier or concurrent run already attempted it.
    AlreadyAttempted { prior: usize },
}

/// Count the source's prior attempts and, only when there are none, insert
/// this run's claim, in one `BEGIN IMMEDIATE` transaction: the write lock is
/// taken before the count, so a concurrent run's count waits for this commit
/// and then sees the claim.
pub fn claim(conn: &Connection, candidate: &Candidate, trigger: &Failure, claimed_at: &str) -> Result<Claim> {
    log::debug!(
        "eval::remediate::claim: trace={} source={} trigger={}",
        candidate.trace,
        candidate.source,
        trigger.check
    );
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .context("opening the remediation claim transaction")?;
    let prior = prior_attempts(&tx, &candidate.source)?;
    if prior > 0 {
        tx.commit().context("closing the remediation claim transaction")?;
        return Ok(Claim::AlreadyAttempted { prior });
    }
    tx.execute(
        "INSERT INTO remediations
         (source_url, original_trace, claimed_at, trigger_judge_model, trigger_check,
          trigger_rubric, trigger_content_hash, trigger_metrics_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            candidate.source,
            candidate.trace,
            claimed_at,
            trigger.judge_model,
            trigger.check,
            trigger.rubric_version,
            trigger.content_hash,
            trigger.metrics_json
        ],
    )
    .with_context(|| format!("claiming the remediation of {}", candidate.source))?;
    tx.commit().context("committing the remediation claim")?;
    Ok(Claim::Claimed)
}

/// The `/ingest` body a remediation posts: a forced re-run of the source with
/// its original method, carrying the lineage the daemon writes at intake.
pub fn request_body(candidate: &Candidate, note: Option<&str>) -> Value {
    let mut body = serde_json::json!({
        "url": candidate.source,
        "tags": [],
        "force": true,
        "method": candidate.method,
        "replay-of": candidate.trace,
        "replay-origin": ReplayOrigin::AutoRemediation,
    });
    if let Some(note) = note {
        body["note"] = Value::String(note.to_string());
    }
    body
}

/// A POST that did not return an `IngestResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostError {
    /// The daemon never accepted the request (no connection, 401, a non-200
    /// answer): no intake row exists, so the claim is released.
    NotDelivered(String),
    /// The request may have reached intake (timeout, unreadable 200): the claim
    /// stays, because an intake row may exist.
    Unknown(String),
}

/// Sends one remediation to the daemon. Production: [`DaemonPoster`].
pub trait Poster {
    fn post(&self, body: &Value) -> impl Future<Output = std::result::Result<IngestResult, PostError>>;
}

/// What happened to one candidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Every failure comes from an untrusted judge check: nothing posted.
    Blocked,
    /// Not a URL receipt: there is no source to re-post.
    NotRemediable(String),
    /// The source already had its one attempt.
    AlreadyAttempted { prior: usize },
    /// Dry run: the body `--apply` would post.
    WouldPost(Value),
    /// Posted; the daemon queued `remediation_trace` with the lineage.
    Posted { remediation_trace: String },
    /// The daemon refused or never got it; the claim was released.
    NotDelivered(String),
    /// Unknown whether intake ran; the claim stays with the error on it.
    Unknown(String),
}

/// One run's results, in candidate order.
#[derive(Debug)]
pub struct Report {
    pub applied: bool,
    pub plan: Plan,
    pub outcomes: Vec<Outcome>,
}

/// Decide every candidate. With no `poster` this is the dry run: it reads the
/// attempt counts and writes nothing. With one, each actionable URL source is
/// claimed and posted. `note_of` resolves a trace's capture note (production:
/// `replay::capture_note_for`); a failure there fails the run before its claim.
pub async fn run<P: Poster>(
    receipts: &Connection,
    plan: Plan,
    poster: Option<&P>,
    note_of: &dyn Fn(&str) -> Result<Option<String>>,
) -> Result<Report> {
    log::debug!(
        "eval::remediate::run: candidates={} apply={}",
        plan.candidates.len(),
        poster.is_some()
    );
    let mut outcomes = Vec::with_capacity(plan.candidates.len());
    for candidate in &plan.candidates {
        let Some(trigger) = candidate.trigger() else {
            outcomes.push(Outcome::Blocked);
            continue;
        };
        if candidate.kind != RECEIPT_KIND_URL {
            outcomes.push(Outcome::NotRemediable(format!(
                "a {} receipt has no URL to re-post",
                candidate.kind
            )));
            continue;
        }
        let Some(poster) = poster else {
            let prior = prior_attempts(receipts, &candidate.source)?;
            outcomes.push(if prior > 0 {
                Outcome::AlreadyAttempted { prior }
            } else {
                let note = note_of(&candidate.trace)?;
                Outcome::WouldPost(request_body(candidate, note.as_deref()))
            });
            continue;
        };
        let note = note_of(&candidate.trace)?;
        let claimed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if let Claim::AlreadyAttempted { prior } = claim(receipts, candidate, trigger, &claimed_at)? {
            outcomes.push(Outcome::AlreadyAttempted { prior });
            continue;
        }
        let outcome = match poster.post(&request_body(candidate, note.as_deref())).await {
            Ok(IngestResult {
                status: IngestStatus::Failed { reason },
                ..
            }) => {
                release(receipts, &candidate.source)?;
                Outcome::NotDelivered(format!("the daemon refused it: {reason}"))
            }
            Ok(IngestResult {
                trace_id: Some(remediation_trace),
                ..
            }) => {
                record_posted(receipts, &candidate.source, &remediation_trace)?;
                Outcome::Posted { remediation_trace }
            }
            Ok(_) => {
                let error = "the daemon answered with no trace id".to_string();
                record_unknown(receipts, &candidate.source, &error)?;
                Outcome::Unknown(error)
            }
            Err(PostError::NotDelivered(error)) => {
                release(receipts, &candidate.source)?;
                Outcome::NotDelivered(error)
            }
            Err(PostError::Unknown(error)) => {
                record_unknown(receipts, &candidate.source, &error)?;
                Outcome::Unknown(error)
            }
        };
        outcomes.push(outcome);
    }
    Ok(Report {
        applied: poster.is_some(),
        plan,
        outcomes,
    })
}

fn release(conn: &Connection, source_url: &str) -> Result<()> {
    log::debug!("eval::remediate::release: source={source_url}");
    conn.execute(
        "DELETE FROM remediations WHERE source_url = ?1 AND remediation_trace IS NULL",
        params![source_url],
    )
    .with_context(|| format!("releasing the remediation claim on {source_url}"))?;
    Ok(())
}

fn record_posted(conn: &Connection, source_url: &str, remediation_trace: &str) -> Result<()> {
    conn.execute(
        "UPDATE remediations SET remediation_trace = ?2, post_error = NULL WHERE source_url = ?1",
        params![source_url, remediation_trace],
    )
    .with_context(|| format!("recording remediation {remediation_trace} of {source_url}"))?;
    Ok(())
}

fn record_unknown(conn: &Connection, source_url: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE remediations SET post_error = ?2 WHERE source_url = ?1",
        params![source_url, error],
    )
    .with_context(|| format!("recording the post error on {source_url}"))?;
    Ok(())
}

/// The remediation trace recorded on `source_url`'s claim, if any.
pub fn remediation_trace_of(conn: &Connection, source_url: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT remediation_trace FROM remediations WHERE source_url = ?1",
        params![source_url],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .with_context(|| format!("reading the remediation claim on {source_url}"))
}

impl Report {
    /// True when a post failed: the run exits non-zero after printing.
    pub fn post_failed(&self) -> bool {
        self.outcomes
            .iter()
            .any(|o| matches!(o, Outcome::NotDelivered(_) | Outcome::Unknown(_)))
    }

    pub fn render(&self) -> String {
        let mut o = String::new();
        let mode = if self.applied { "applied" } else { "dry run; --apply posts" };
        let actionable = self.plan.candidates.iter().filter(|c| c.trigger().is_some()).count();
        o.push_str(&format!(
            "remediation ({mode})  scored traces {}  failing {}  actionable {}\n",
            self.plan.scored,
            self.plan.candidates.len(),
            actionable
        ));
        o.push_str(&format!(
            "not read: {} swept from staging, {} with no succeeded receipt here, {} unreadable\n",
            self.plan.not_staged,
            self.plan.no_receipt,
            self.plan.unreadable.len()
        ));
        for (trace, why) in &self.plan.unreadable {
            o.push_str(&format!("  unreadable {trace}: {why}\n"));
        }
        for (candidate, outcome) in self.plan.candidates.iter().zip(&self.outcomes) {
            o.push_str(&format!(
                "\n{}  {}  [{} via {}]\n",
                candidate.trace, candidate.source, candidate.kind, candidate.method
            ));
            for f in &candidate.failures {
                o.push_str(&format!("  FAIL {}: {}\n", failure_label(f), f.reason));
                if let TrustState::Untrusted(why) = &f.gate {
                    // One line per class, its reasons joined, in gate order.
                    let mut classes: Vec<(String, Vec<&str>)> = Vec::new();
                    for w in why {
                        let class = w.class.to_string();
                        match classes.iter_mut().find(|(c, _)| *c == class) {
                            Some((_, reasons)) => reasons.push(&w.reason),
                            None => classes.push((class, vec![&w.reason])),
                        }
                    }
                    for (class, reasons) in classes {
                        o.push_str(&format!(
                            "       gate: judge {} check {} class {class} untrusted ({})\n",
                            f.judge_model,
                            f.check,
                            reasons.join("; ")
                        ));
                    }
                }
            }
            o.push_str(&format!("  -> {}\n", outcome_line(outcome)));
        }
        let tally = |pred: fn(&Outcome) -> bool| self.outcomes.iter().filter(|o| pred(o)).count();
        o.push_str(&format!(
            "\nposted {}  would-post {}  blocked (untrusted judge only) {}  already attempted {}  not remediable {}  not delivered {}  unknown {}\n",
            tally(|o| matches!(o, Outcome::Posted { .. })),
            tally(|o| matches!(o, Outcome::WouldPost(_))),
            tally(|o| matches!(o, Outcome::Blocked)),
            tally(|o| matches!(o, Outcome::AlreadyAttempted { .. })),
            tally(|o| matches!(o, Outcome::NotRemediable(_))),
            tally(|o| matches!(o, Outcome::NotDelivered(_))),
            tally(|o| matches!(o, Outcome::Unknown(_))),
        ));
        o
    }
}

fn failure_label(f: &Failure) -> String {
    match &f.gate {
        TrustState::Ungated => format!("{} (deterministic, ungated)", f.check),
        TrustState::Trusted => format!("{} {} (trusted judge)", f.check, f.judge_model),
        TrustState::Untrusted(_) => format!("{} {} (untrusted judge: recorded, never acts)", f.check, f.judge_model),
    }
}

fn outcome_line(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Blocked => "not posted: only an untrusted judge check fails it".to_string(),
        Outcome::NotRemediable(why) => format!("not remediable: {why}"),
        Outcome::AlreadyAttempted { prior } => {
            format!("not posted: the source already had its one remediation ({prior} prior attempt(s))")
        }
        Outcome::WouldPost(body) => format!("would post {body}"),
        Outcome::Posted { remediation_trace } => format!("posted, remediation trace {remediation_trace}"),
        Outcome::NotDelivered(why) => format!("NOT DELIVERED, claim released: {why}"),
        Outcome::Unknown(why) => format!("UNKNOWN whether intake ran, claim kept: {why}"),
    }
}

/// The production [`Poster`]: the borg daemon's `/ingest`.
pub struct DaemonPoster {
    daemon: DaemonClient,
}

impl DaemonPoster {
    /// Connect, and refuse a daemon whose `/health` does not advertise
    /// [`FEATURE_REPLAY_OF`]: an older daemon would queue the ingest and drop
    /// `replay-of`, a remediation with no lineage.
    pub async fn connect(daemon: DaemonClient) -> Result<Self> {
        let resp = daemon
            .get("/health", &[], None)
            .await
            .with_context(|| format!("checking the daemon at {} before remediating", daemon.addr()))?;
        let health: HealthResponse = daemon.json("/health", resp).await?;
        log::debug!(
            "eval::remediate::DaemonPoster::connect: version={} features={:?}",
            health.version,
            health.features
        );
        if !health.supports(FEATURE_REPLAY_OF) {
            eyre::bail!(
                "the daemon at {} (version {}) does not record replay-of at intake; deploy this sb \
                 (`otto deploy`) before `--remediate --apply`, or its remediations would carry no lineage",
                daemon.addr(),
                health.version
            );
        }
        Ok(Self { daemon })
    }
}

/// Which side of intake a daemon error lands on.
pub fn classify(e: &DaemonError) -> PostError {
    let message = e.to_string();
    match e {
        // Refused before the handler ran (auth layer, body extractor), or
        // never sent.
        DaemonError::Unauthorized { .. } | DaemonError::BadAddress { .. } | DaemonError::Client { .. } => {
            PostError::NotDelivered(message)
        }
        DaemonError::Status { status, .. } if *status < 500 => PostError::NotDelivered(message),
        DaemonError::Unreachable { .. } if e.is_connect() => PostError::NotDelivered(message),
        // A 5xx, a dropped response or an unreadable 200 can follow intake.
        DaemonError::Status { .. } | DaemonError::Unreachable { .. } | DaemonError::Parse { .. } => {
            PostError::Unknown(message)
        }
    }
}

impl Poster for DaemonPoster {
    async fn post(&self, body: &Value) -> std::result::Result<IngestResult, PostError> {
        let resp = self.daemon.post_json("/ingest", body).await.map_err(|e| classify(&e))?;
        self.daemon
            .json::<IngestResult>("/ingest", resp)
            .await
            .map_err(|e| classify(&e))
    }
}

#[cfg(test)]
mod tests;
