//! Phase-7 `cortex summarize --backfill` subcommand.
//!
//! Walks the vault, infers an `IngestKind` from each note's frontmatter +
//! source URL, invokes the shared `distillers` crate, and rewrites the
//! vault note file (atomic .tmp + rename) with the rendered `Distilled`
//! body sections and frontmatter additions. VaultWatcher (oracle) picks
//! up the mtime change and reindexes; cortex never writes SQLite.
//!
//! Concurrency is bounded by `config.backfill.max-concurrent` (default 2).
//! Resume state is a single JSON file tracking the last completed note
//! path; the next run skips up to and including that path when `--resume`
//! is in effect.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use eyre::{Context, Result, eyre};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ::vault::distilled::Distilled;
use distillers::{
    ArticleConfig, Dispatch, Dispatcher, DistillInputs, DistillKind, FabricCaller, FabricShell, demote_headings, render,
};

use crate::config::Config;
use crate::opts::SummarizeOpts;
use crate::vault::{Frontmatter, Note, scan_vault};

/// Match the design doc's "log every 100 notes" cadence.
const PROGRESS_LOG_EVERY: u64 = 100;

/// Top-level orchestrator for `sb cortex summarize`. Logs the command start
/// before delegating to the backfill core.
pub async fn run(vault_root: &Path, config: &Config, opts: &SummarizeOpts) -> Result<BackfillSummary> {
    log::info!("starting summarize command (vault_root={})", vault_root.display());
    backfill(vault_root, config, opts).await
}

/// Build a `FabricShell`-backed dispatcher and delegate to the generic core so
/// tests can inject a fake.
pub async fn backfill(vault_root: &Path, config: &Config, opts: &SummarizeOpts) -> Result<BackfillSummary> {
    log::debug!(
        "summarize::backfill: vault_root={} since={:?} domain={:?} extractor={:?} dry_run={} resume={}",
        vault_root.display(),
        opts.since,
        opts.domain,
        opts.extractor,
        opts.dry_run,
        opts.resume,
    );

    if !opts.backfill {
        return Err(eyre!(
            "cortex summarize requires --backfill; no other modes are implemented yet"
        ));
    }

    let fabric = FabricShell::new(config.fabric.binary.clone());
    let article_config = ArticleConfig {
        model: config.fabric.model.clone(),
        max_chars: config.fabric.max_content_chars,
        timeout_secs: config.fabric.timeout_secs,
    };
    let dispatcher = Dispatcher::new(fabric, article_config);
    backfill_with_dispatcher(vault_root, config, opts, dispatcher).await
}

/// Test-injectable core. Production builds a `FabricShell` dispatcher;
/// tests build a `Dispatcher<Arc<FakeFabric>>` and reuse the same logic.
pub async fn backfill_with_dispatcher<F: FabricCaller + Clone + Send + Sync + 'static>(
    vault_root: &Path,
    config: &Config,
    opts: &SummarizeOpts,
    dispatcher: Dispatcher<F>,
) -> Result<BackfillSummary> {
    let notes = scan_vault(vault_root, &config.vault).context("scan vault")?;
    log::info!("summarize::backfill: scanned {} notes", notes.len());

    let checkpoint_path = checkpoint_path(vault_root, config);
    let resume_after = if opts.resume { load_checkpoint(&checkpoint_path) } else { None };
    if let Some(ref last) = resume_after {
        log::info!(
            "summarize::backfill: resume from checkpoint last_path={}",
            last.display()
        );
    } else {
        log::info!("summarize::backfill: starting fresh (no checkpoint or --no-resume)");
    }

    let candidates: Vec<Note> = filter_notes(&notes, opts, resume_after.as_deref());
    log::info!(
        "summarize::backfill: {} note(s) qualify after filters (since={:?} domain={:?} extractor={:?})",
        candidates.len(),
        opts.since,
        opts.domain,
        opts.extractor,
    );

    if candidates.is_empty() {
        log::info!("summarize::backfill: nothing to do");
        return Ok(BackfillSummary::default());
    }

    if opts.dry_run {
        for note in &candidates {
            println!(
                "[would-distill] {} (kind={:?})",
                note.path.display(),
                infer_distill_kind(note).map(|k| k.as_str())
            );
        }
        return Ok(BackfillSummary {
            attempted: candidates.len() as u64,
            distilled: 0,
            skipped: 0,
            failed: 0,
        });
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(
        config.backfill.max_concurrent.max(1) as usize
    ));
    let dispatcher = Arc::new(dispatcher);
    let attempted = Arc::new(AtomicU64::new(0));
    let distilled_count = Arc::new(AtomicU64::new(0));
    let skipped = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let total = candidates.len();

    // Per-note tasks run concurrently up to `max_concurrent`. Checkpoint
    // writes after each successful rewrite so a crashed run resumes near
    // where it stopped.
    let mut handles = Vec::with_capacity(candidates.len());
    for note in candidates {
        let permit_owner = semaphore.clone();
        let dispatcher = dispatcher.clone();
        let vault_root = vault_root.to_path_buf();
        let checkpoint_path = checkpoint_path.clone();
        let attempted = attempted.clone();
        let distilled_count = distilled_count.clone();
        let skipped = skipped.clone();
        let failed = failed.clone();
        let extractor_override = opts.extractor.clone();
        let handle = tokio::spawn(async move {
            let _permit = match permit_owner.acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("summarize::backfill: semaphore closed mid-flight: {e}");
                    return;
                }
            };
            let path = note.path.clone();
            attempted.fetch_add(1, Ordering::Relaxed);
            match process_one(&vault_root, &note, dispatcher.as_ref(), extractor_override.as_deref()).await {
                Ok(ProcessOutcome::Distilled) => {
                    let _ = save_checkpoint(&checkpoint_path, &path);
                    let done = distilled_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if done.is_multiple_of(PROGRESS_LOG_EVERY) {
                        log::info!(
                            "summarize::backfill: progress {}/{} distilled (failed={})",
                            done,
                            total,
                            failed.load(Ordering::Relaxed),
                        );
                    }
                }
                Ok(ProcessOutcome::Skipped(reason)) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    log::debug!("summarize::backfill: skip {}: {reason}", path.display());
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    log::warn!("summarize::backfill: failed {}: {e:#}", path.display());
                }
            }
        });
        handles.push(handle);
    }
    for h in handles {
        let _ = h.await;
    }

    let summary = BackfillSummary {
        attempted: attempted.load(Ordering::Relaxed),
        distilled: distilled_count.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
    };
    log::info!(
        "summarize::backfill: complete attempted={} distilled={} skipped={} failed={}",
        summary.attempted,
        summary.distilled,
        summary.skipped,
        summary.failed,
    );
    Ok(summary)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillSummary {
    pub attempted: u64,
    pub distilled: u64,
    pub skipped: u64,
    pub failed: u64,
}

enum ProcessOutcome {
    Distilled,
    Skipped(&'static str),
}

async fn process_one<F: FabricCaller + Clone>(
    vault_root: &Path,
    note: &Note,
    dispatcher: &Dispatcher<F>,
    extractor_override: Option<&str>,
) -> Result<ProcessOutcome> {
    log::debug!(
        "summarize::process_one: path={} note_type={:?} source={:?}",
        note.path.display(),
        note.frontmatter.note_type,
        note.frontmatter.source
    );

    if extractor_override.is_none() && is_already_distilled(&note.frontmatter) {
        return Ok(ProcessOutcome::Skipped("already distilled"));
    }

    let Some(kind) = infer_distill_kind(note) else {
        return Ok(ProcessOutcome::Skipped("unrecognised note type"));
    };

    if matches!(kind, DistillKind::Idea | DistillKind::Image | DistillKind::VoiceNote) && note.body.trim().is_empty() {
        return Ok(ProcessOutcome::Skipped("no body content to distill"));
    }

    // Backfill reads the entire legacy note body as the "transcript" input.
    // For video/voicenote kinds the distiller preserves that input verbatim
    // inside Distilled.transcript, and render.rs emits it under `## Transcript`.
    // Any H1/H2 in the legacy body would collide with the new L2 section
    // headings (## Summary / ## Claims / ## Links), so we demote them two
    // levels here at the source. See docs/design/2026-05-18-fabric-pattern-
    // resolve-and-distill-dlq.md follow-up.
    let demoted_body = demote_headings(&note.body, 2);
    let inputs = DistillInputs {
        transcript: demoted_body.as_str(),
        source_url: note.frontmatter.source.as_deref(),
        title_hint: note.frontmatter.title.as_deref(),
        repo_metadata: None,
        video_metadata: None,
    };

    let distilled = dispatcher.distill(kind, inputs).await.context("dispatch distill")?;
    let absolute = absolute_note_path(vault_root, &note.path);
    rewrite_note_file(&absolute, &note.frontmatter, &distilled)?;
    Ok(ProcessOutcome::Distilled)
}

/// Rewrite a vault note in place by merging the rendered Distilled into the
/// existing frontmatter and replacing the body. Atomic: writes to
/// `<path>.tmp` and renames into place so a crash mid-write never leaves a
/// half-rewritten file.
pub fn rewrite_note_file(path: &Path, base_frontmatter: &Frontmatter, distilled: &Distilled) -> Result<()> {
    let rendered = render(distilled);
    let mut merged = clone_frontmatter(base_frontmatter);
    for (k, v) in rendered.frontmatter_additions {
        merged.extra.insert(k, v);
    }
    let yaml = merged.to_yaml().context("serialize frontmatter")?;
    let content = format!("---\n{yaml}---\n{}", rendered.body_markdown);
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    log::debug!(
        "summarize::rewrite_note_file: path={} extractor={}",
        path.display(),
        distilled.meta.extractor
    );
    Ok(())
}

fn clone_frontmatter(fm: &Frontmatter) -> Frontmatter {
    Frontmatter {
        title: fm.title.clone(),
        date: fm.date.clone(),
        note_type: fm.note_type.clone(),
        domain: fm.domain.clone(),
        origin: fm.origin.clone(),
        status: fm.status.clone(),
        tags: fm.tags.clone(),
        source: fm.source.clone(),
        creator: fm.creator.clone(),
        pinned: fm.pinned,
        extra: fm.extra.clone(),
    }
}

/// Resolve a vault-relative `Note::path` to an absolute filesystem path.
/// Absolute paths pass through unchanged so a `Note` constructed manually in
/// tests can already carry the absolute path.
fn absolute_note_path(vault_root: &Path, relative: &Path) -> PathBuf {
    if relative.is_absolute() {
        relative.to_path_buf()
    } else {
        vault_root.join(relative)
    }
}

/// Returns true when the note already carries the Phase-7 skip marker.
pub fn is_already_distilled(fm: &Frontmatter) -> bool {
    fm.extra.get("distilled").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Map a note's frontmatter `type:` to the distillers crate's kind. Returns
/// `None` for system / daily / MOC / etc. - kinds borg never ingests as L2
/// sources. Falls back to the source-URL host when `type:` is missing or
/// generic (e.g. `note`).
pub fn infer_distill_kind(note: &Note) -> Option<DistillKind> {
    if let Some(kind) = note.frontmatter.note_type.as_deref().and_then(kind_from_type) {
        return Some(kind);
    }
    if let Some(url) = note.frontmatter.source.as_deref() {
        return kind_from_url(url);
    }
    None
}

fn kind_from_type(t: &str) -> Option<DistillKind> {
    match t.to_ascii_lowercase().as_str() {
        "article" => Some(DistillKind::Article),
        "github" => Some(DistillKind::Repo),
        "youtube" | "video" => Some(DistillKind::Video),
        "social" | "reddit" => Some(DistillKind::Thread),
        "image" | "pdf" | "document" => Some(DistillKind::Image),
        "audio" => Some(DistillKind::VoiceNote),
        "note" => Some(DistillKind::Idea),
        _ => None,
    }
}

fn kind_from_url(url: &str) -> Option<DistillKind> {
    let host = ::url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))?;
    if host.ends_with("github.com") {
        return Some(DistillKind::Repo);
    }
    if host.ends_with("youtube.com") || host == "youtu.be" || host.ends_with(".youtube.com") {
        return Some(DistillKind::Video);
    }
    if host == "x.com"
        || host.ends_with(".x.com")
        || host == "twitter.com"
        || host.ends_with(".twitter.com")
        || host.ends_with("reddit.com")
        || host.ends_with("news.ycombinator.com")
    {
        return Some(DistillKind::Thread);
    }
    Some(DistillKind::Article)
}

/// Apply `--since` and `--domain` to the scanned notes; drop notes that
/// fall before the resume-checkpoint path. The result is a freshly-owned
/// vector so the spawned tasks can move each note independently.
pub fn filter_notes(notes: &[Note], opts: &SummarizeOpts, resume_after: Option<&Path>) -> Vec<Note> {
    let cutoff = opts.since.as_deref().and_then(parse_since);
    let domain = opts.domain.as_deref();
    let mut past_resume = resume_after.is_none();
    let mut out = Vec::new();
    for note in notes {
        if !past_resume {
            if let Some(checkpoint) = resume_after
                && note.path == checkpoint
            {
                past_resume = true;
            }
            continue;
        }
        // Backfill targets only borg-INGESTED notes. Authored frontmatter -
        // `origin: authored` (CLAUDE.md, home.md, MOCs, system views) or
        // `origin: human` (daily journals) - is out of scope and must never
        // enter the candidate pool. Filtering here (instead of relying on
        // per-note skip-at-scan) keeps the candidate count honest and avoids
        // log noise from 142+ "unrecognised note type" skips per run.
        if note.frontmatter.origin.as_deref() != Some("assisted") {
            continue;
        }
        if let Some(cutoff) = cutoff
            && !note_date_at_or_after(note, cutoff)
        {
            continue;
        }
        if let Some(want) = domain
            && note.frontmatter.domain.as_deref() != Some(want)
        {
            continue;
        }
        out.push(note.clone());
    }
    out
}

/// Parse `--since` values like `30d`, `2w`, `3mo`. Returns the cutoff
/// `DateTime<Utc>` (today minus the duration). Unrecognised suffixes return
/// None so the filter is a no-op rather than silently misbehaving.
pub fn parse_since(spec: &str) -> Option<DateTime<Utc>> {
    let trimmed = spec.trim();
    let (num_part, unit) = split_since(trimmed)?;
    let n: i64 = num_part.parse().ok()?;
    let days = match unit {
        "d" => n,
        "w" => n.checked_mul(7)?,
        "mo" => n.checked_mul(30)?,
        _ => return None,
    };
    Utc::now().checked_sub_signed(Duration::days(days))
}

fn split_since(spec: &str) -> Option<(&str, &str)> {
    if let Some(idx) = spec.find(|c: char| !c.is_ascii_digit()) {
        let (num, unit) = spec.split_at(idx);
        if num.is_empty() {
            return None;
        }
        return Some((num, unit));
    }
    None
}

/// Whether a note's `date:` is at or after `cutoff`. Notes with an invalid
/// or missing date fall through as "kept" so the user doesn't silently
/// lose coverage on legacy notes that pre-date the date convention.
pub fn note_date_at_or_after(note: &Note, cutoff: DateTime<Utc>) -> bool {
    let Some(date_str) = note.frontmatter.date.as_deref() else {
        return true;
    };
    let Some(date) = parse_yyyy_mm_dd(date_str) else {
        return true;
    };
    let datetime = Utc
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
        .single();
    match datetime {
        Some(dt) => dt >= cutoff,
        None => true,
    }
}

fn parse_yyyy_mm_dd(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Checkpoint {
    /// Vault-relative path of the last successfully-rewritten note.
    last_completed: Option<PathBuf>,
    /// Free-form metadata so future fields can be added without breaking
    /// older state files.
    #[serde(default)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// Resolve the absolute path of the resume checkpoint file inside the
/// vault's `state.cache-dir` (default `.cortex/<filename>`).
pub fn checkpoint_path(vault_root: &Path, config: &Config) -> PathBuf {
    let cache_dir = vault_root.join(&config.state.cache_dir);
    cache_dir.join(&config.backfill.checkpoint_file)
}

fn load_checkpoint(path: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(path).ok()?;
    let parsed: Checkpoint = serde_json::from_slice(&bytes).ok()?;
    parsed.last_completed
}

fn save_checkpoint(path: &Path, last: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create checkpoint dir {}", parent.display()))?;
    }
    let cp = Checkpoint {
        last_completed: Some(last.to_path_buf()),
        extra: BTreeMap::new(),
    };
    let json = serde_json::to_vec_pretty(&cp).context("serialize checkpoint")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
