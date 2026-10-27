//! Multi-target `sb borg ingest`: classify every target before any submit,
//! then submit URL targets through the daemon and file targets through the
//! local pipeline. Returns typed data; `sb` prints and picks the exit code.

use std::future::Future;
use std::path::{Path, PathBuf};

use eyre::Result;

use super::{IngestOutcome, post_ingest};
use crate::config::Config;
use crate::types;

/// One classified ingest target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An `http(s)` URL: submitted to the daemon's `/ingest`.
    Url(String),
    /// An existing regular file: run through the local pipeline.
    File(PathBuf),
}

/// The run cannot start: nothing was submitted. `sb` exits 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn is_http_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Classify every target (http(s) URL | existing file) and reject the shapes
/// that would silently misfire: a tag that is really a target the greedy `-t`
/// swallowed, and `--note` alongside a file target.
pub fn classify_targets(
    targets: &[String],
    tags: Option<&[String]>,
    note: Option<&str>,
) -> Result<Vec<Target>, UsageError> {
    if targets.is_empty() {
        return Err(UsageError("no targets given: pass 1+ URLs or file paths".to_string()));
    }

    let swallowed: Vec<&String> = tags
        .unwrap_or_default()
        .iter()
        .filter(|t| is_http_url(t) || Path::new(t.as_str()).exists())
        .collect();
    if !swallowed.is_empty() {
        let names: Vec<&str> = swallowed.iter().map(|t| t.as_str()).collect();
        return Err(UsageError(format!(
            "--tags swallowed what looks like a target ({}); put the targets before -t/--tags",
            names.join(", ")
        )));
    }

    let mut classified = Vec::with_capacity(targets.len());
    let mut bad = Vec::new();
    for t in targets {
        if is_http_url(t) {
            classified.push(Target::Url(t.clone()));
        } else if Path::new(t).is_file() {
            classified.push(Target::File(PathBuf::from(t)));
        } else if Path::new(t).exists() {
            bad.push(format!("{t} (exists but is not a file)"));
        } else {
            bad.push(format!("{t} (not an http(s) URL and no such file)"));
        }
    }
    if !bad.is_empty() {
        return Err(UsageError(format!("cannot ingest: {}", bad.join("; "))));
    }

    if note.is_some_and(|n| !n.trim().is_empty()) && classified.iter().any(|t| matches!(t, Target::File(_))) {
        return Err(UsageError(
            "--note applies to URL targets, not file targets".to_string(),
        ));
    }
    Ok(classified)
}

/// What happened to one target.
#[derive(Debug)]
pub struct TargetReport {
    /// The target as the operator typed it.
    pub target: String,
    pub is_url: bool,
    /// `Err` carries the reason a submit or local ingest errored.
    pub result: Result<IngestOutcome, String>,
}

impl TargetReport {
    pub fn failed(&self) -> bool {
        matches!(self.result, Err(_) | Ok(IngestOutcome::Failed { .. }))
    }
}

/// The whole run, in execution order (URL targets, then file targets).
#[derive(Debug, Default)]
pub struct IngestRun {
    pub reports: Vec<TargetReport>,
    /// URL targets submitted.
    pub submits: usize,
    /// URL targets whose submit failed.
    pub failed_submits: usize,
}

impl IngestRun {
    pub fn any_failed(&self) -> bool {
        self.reports.iter().any(TargetReport::failed)
    }
}

/// Run classified targets: every URL first (so a slow file never delays the
/// daemon's queue), then each file through `ingest_file`.
///
/// `toast` is the desktop-toast seam. A single target keeps the per-failure
/// toast (hotkey/clipboard shape); a multi-target run suppresses it per target
/// and sends at most one `<k> of <n> submits failed` toast. `on_report` fires
/// as each target finishes so `sb` can print live.
pub async fn ingest_targets<F, Fut>(
    config: &Config,
    targets: Vec<Target>,
    tags: Option<Vec<String>>,
    force: bool,
    note: Option<String>,
    toast: impl Fn(&str, &str),
    mut ingest_file: F,
    mut on_report: impl FnMut(&TargetReport),
) -> IngestRun
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = Result<IngestOutcome>>,
{
    let multi = targets.len() > 1;
    let quiet = |_: &str, _: &str| {};
    let per_target: &dyn Fn(&str, &str) = if multi { &quiet } else { &toast };

    let (urls, files): (Vec<Target>, Vec<Target>) = targets.into_iter().partition(|t| matches!(t, Target::Url(_)));
    let mut run = IngestRun::default();

    for target in urls {
        let Target::Url(url) = target else { continue };
        let result = post_ingest(
            config,
            &url,
            tags.clone(),
            force,
            types::IngestMethod::Cli,
            note.as_deref(),
            per_target,
        )
        .await
        .map_err(|e| format!("{e:#}"));
        run.submits += 1;
        let report = TargetReport {
            target: url,
            is_url: true,
            result,
        };
        if report.failed() {
            run.failed_submits += 1;
        }
        on_report(&report);
        run.reports.push(report);
    }

    for target in files {
        let Target::File(path) = target else { continue };
        let result = ingest_file(path.clone()).await.map_err(|e| format!("{e:#}"));
        let report = TargetReport {
            target: path.display().to_string(),
            is_url: false,
            result,
        };
        on_report(&report);
        run.reports.push(report);
    }

    if multi && run.failed_submits > 0 {
        toast(
            &format!("{} of {} submits failed", run.failed_submits, run.submits),
            "see the terminal for each target's reason",
        );
    }
    run
}

#[cfg(test)]
mod tests;
