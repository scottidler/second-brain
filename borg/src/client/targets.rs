//! Multi-target `sb borg ingest`: classify every target before any submit,
//! then submit URL targets through the daemon and file targets through the
//! local pipeline. Returns typed data; `sb` prints and picks the exit code.

use std::future::Future;
use std::path::{Path, PathBuf};

use eyre::Result;

use super::{IngestOutcome, post_ingest, seal};
use crate::assets;
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

/// A strict absolute http(s) URL: literal `http://`/`https://` prefix (any
/// case), a host, and no whitespace, control character, or backslash.
/// `Url::parse` alone is too forgiving: it percent-encodes spaces and
/// normalizes `http:host` and backslashes instead of rejecting them. A bare
/// `%` passes, as it does in a browser.
pub fn is_http_url(s: &str) -> bool {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return false;
    }
    url::Url::parse(s).is_ok_and(|u| u.host_str().is_some_and(|h| !h.is_empty()))
}

/// Shaped like a URL (`scheme://...`): it must validate as one, never fall
/// through to a file lookup.
fn looks_like_url(s: &str) -> bool {
    s.contains("://")
}

/// Parse a URL list: one URL per line; blank lines and lines whose first
/// non-blank character is `#` are skipped. Every other line must be an
/// http(s) URL (a file path is not followed). `source` names the list in
/// errors as `source:line`.
pub fn parse_url_list(text: &str, source: &str) -> Result<Vec<String>, UsageError> {
    let mut urls = Vec::new();
    let mut bad = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if is_http_url(line) {
            urls.push(line.to_string());
        } else {
            bad.push(format!(
                "{source}:{}: {line} (not an http(s) URL; a list holds URLs only)",
                idx + 1
            ));
        }
    }
    if !bad.is_empty() {
        return Err(UsageError(format!("cannot ingest: {}", bad.join("; "))));
    }
    if urls.is_empty() {
        return Err(UsageError(format!("{source}: no URLs in the list")));
    }
    Ok(urls)
}

fn is_content_file(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    assets::is_image_extension(&name)
        || assets::is_pdf_extension(&name)
        || assets::is_document_extension(&name)
        || assets::is_audio_extension(&name)
}

/// Resolve every input to concrete targets before anything is submitted.
///
/// - `items`: each is an http(s) URL, a URL-list file (every line a URL), or
///   `-` (a URL list on stdin, read through `read_stdin`, at most once).
/// - `files`: `--file` paths, each a supported content file ingested as a note.
///
/// Any malformed item, unreadable or empty list, bad list line, or bad
/// `--file` is a `UsageError` naming it; then nothing runs. Also rejects a tag
/// that is really a target the greedy `-t` swallowed, and `--note` alongside
/// `--file`. URLs come out in input order, list lines expanded in place.
pub fn classify_targets(
    items: &[String],
    files: &[PathBuf],
    tags: Option<&[String]>,
    note: Option<&str>,
    read_stdin: impl FnOnce() -> std::io::Result<String>,
) -> Result<Vec<Target>, UsageError> {
    if items.is_empty() && files.is_empty() {
        return Err(UsageError(
            "no targets given: pass 1+ URLs or URL-list files, or --file <FILE>".to_string(),
        ));
    }

    let mut bad = Vec::new();
    let swallowed: Vec<&str> = tags
        .unwrap_or_default()
        .iter()
        .filter(|t| looks_like_url(t) || vault::paths::expand_tilde(t).exists())
        .map(String::as_str)
        .collect();
    if !swallowed.is_empty() {
        bad.push(format!(
            "--tags swallowed what looks like a target ({}); put the targets before -t/--tags",
            swallowed.join(", ")
        ));
    }

    let stdin_twice = items.iter().filter(|i| i.as_str() == "-").count() > 1;
    if stdin_twice {
        bad.push("`-` (stdin) can be given only once".to_string());
    }

    // Any `--note`, blank included: the flag itself is the mixed-run misuse.
    if note.is_some() && !files.is_empty() {
        bad.push("--note applies to URLs, not --file content".to_string());
    }

    let mut read_stdin = (!stdin_twice).then_some(read_stdin);
    let mut classified = Vec::new();
    for item in items {
        if looks_like_url(item) || is_http_url(item) {
            match is_http_url(item) {
                true => classified.push(Target::Url(item.clone())),
                false => bad.push(format!("{item} (malformed URL)")),
            }
            continue;
        }
        let listed = if item == "-" {
            let Some(read) = read_stdin.take() else { continue };
            read()
                .map_err(|e| UsageError(format!("-: {e}")))
                .and_then(|text| parse_url_list(&text, "-"))
        } else {
            let path = vault::paths::expand_tilde(item);
            if path.is_file() {
                if is_content_file(&path) {
                    bad.push(format!(
                        "{item} (a content file; use --file {item} to ingest it as a note)"
                    ));
                    continue;
                }
                std::fs::read_to_string(&path)
                    .map_err(|e| UsageError(format!("{item}: {e}")))
                    .and_then(|text| parse_url_list(&text, item))
            } else if path.exists() {
                Err(UsageError(format!("{item} (exists but is not a file)")))
            } else {
                Err(UsageError(format!("{item} (not an http(s) URL and no such file)")))
            }
        };
        match listed {
            Ok(urls) => classified.extend(urls.into_iter().map(Target::Url)),
            Err(e) => bad.push(e.0.trim_start_matches("cannot ingest: ").to_string()),
        }
    }

    for file in files {
        let path = vault::paths::expand_tilde(file);
        if !path.is_file() {
            bad.push(format!("--file {} (no such file)", file.display()));
        } else if !is_content_file(&path) {
            bad.push(format!(
                "--file {} (unsupported content type; a URL list goes in as a plain argument)",
                file.display()
            ));
        } else {
            classified.push(Target::File(path));
        }
    }

    if !bad.is_empty() {
        return Err(UsageError(format!("cannot ingest: {}", bad.join("; "))));
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

/// What happened to a bulk's seal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealStatus {
    /// The daemon accepted the seal of this many queued URL items.
    Sealed { items: usize },
    /// The seal POST failed; the watchdog will summarize as "seal missing".
    Failed(String),
    /// No URL item came back `Queued`, so there was nothing to seal.
    NothingQueued,
}

/// A bulk's batch: its id and its seal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReport {
    pub id: String,
    pub seal: SealStatus,
}

/// The whole run, in execution order (URL targets, then file targets).
#[derive(Debug, Default)]
pub struct IngestRun {
    pub reports: Vec<TargetReport>,
    /// URL targets submitted.
    pub submits: usize,
    /// URL targets whose submit failed.
    pub failed_submits: usize,
    /// This run is a bulk per `is_bulk`.
    pub bulk: bool,
    /// Set for a bulk: the batch its URL items carried and how its seal went.
    pub batch: Option<BatchReport>,
}

impl IngestRun {
    /// Any target failed, or the bulk's seal failed: `sb` exits 1.
    pub fn any_failed(&self) -> bool {
        self.reports.iter().any(TargetReport::failed)
            || matches!(
                self.batch,
                Some(BatchReport {
                    seal: SealStatus::Failed(_),
                    ..
                })
            )
    }
}

/// The bulk rule: more than `threshold` targets AND at least one URL target.
/// Threshold 0 makes every run with a URL target a bulk. `--clipboard` is
/// never a bulk, and a file-only run never opens a batch.
pub fn is_bulk(url_count: usize, total_targets: usize, threshold: u32, clipboard: bool) -> bool {
    !clipboard && url_count > 0 && total_targets > threshold as usize
}

/// Run classified targets: every URL first (so a slow file never delays the
/// daemon's queue), then each file through `ingest_file`.
///
/// A bulk (`is_bulk`) first refuses a daemon without `bulk-batch` (an `Err`,
/// nothing submitted), sends a fresh batch id on every URL POST, and seals the
/// batch with the queued trace ids before any file target runs. A failed seal
/// is reported in `IngestRun.batch`; file targets still run.
///
/// `toast` is the desktop-toast seam. A single target keeps the per-failure
/// toast (hotkey/clipboard shape); a multi-target run suppresses it per target
/// and sends at most one `<k> of <n> submits failed` toast. `on_report` fires
/// as each target finishes so `sb` can print live.
#[allow(clippy::too_many_arguments)]
pub async fn ingest_targets<F, Fut>(
    config: &Config,
    targets: Vec<Target>,
    tags: Option<Vec<String>>,
    force: bool,
    note: Option<String>,
    toast: impl Fn(&str, &str),
    mut ingest_file: F,
    mut on_report: impl FnMut(&TargetReport),
) -> Result<IngestRun>
where
    F: FnMut(PathBuf) -> Fut,
    Fut: Future<Output = Result<IngestOutcome>>,
{
    let multi_total = targets.len();
    let multi = multi_total > 1;
    let quiet = |_: &str, _: &str| {};
    let per_target: &dyn Fn(&str, &str) = if multi { &quiet } else { &toast };

    let (urls, files): (Vec<Target>, Vec<Target>) = targets.into_iter().partition(|t| matches!(t, Target::Url(_)));
    // `--clipboard` returns in `sb` before reaching this function.
    let mut run = IngestRun {
        bulk: is_bulk(urls.len(), multi_total, config.ingest.bulk_threshold, false),
        ..IngestRun::default()
    };
    let batch_id = if run.bulk {
        seal::require_bulk_feature(config).await?;
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };
    log::debug!(
        "ingest_targets: urls={} files={} bulk={} batch={batch_id:?}",
        urls.len(),
        files.len(),
        run.bulk
    );

    let mut queued = Vec::new();
    for target in urls {
        let Target::Url(url) = target else { continue };
        let result = post_ingest(
            config,
            &url,
            tags.clone(),
            force,
            types::IngestMethod::Cli,
            note.as_deref(),
            batch_id.as_deref(),
            per_target,
        )
        .await
        .map_err(|e| format!("{e:#}"));
        run.submits += 1;
        if let Ok(IngestOutcome::Queued { trace_id }) = &result {
            queued.push(trace_id.clone());
        }
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

    if let Some(id) = batch_id {
        let seal = if queued.is_empty() {
            SealStatus::NothingQueued
        } else {
            match seal::seal_batch(config, &id, &queued).await {
                Ok(()) => SealStatus::Sealed { items: queued.len() },
                Err(e) => {
                    log::warn!("ingest_targets: seal of batch {id} failed: {e:#}");
                    SealStatus::Failed(format!("{e:#}"))
                }
            }
        };
        run.batch = Some(BatchReport { id, seal });
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
    Ok(run)
}

#[cfg(test)]
mod tests;
