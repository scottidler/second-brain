//! The YouTube frames work dir: its lifetime guard, the copy that preserves it
//! under `PublishPolicy::Scratch`, and the `runs.yml` record of every
//! content-filter run (docs/design/2026-10-25-borg-distill.md).

use eyre::{Context, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{SlideCategory, SlideClass};
use crate::slides::Run;
use crate::slides::classify::ClassifyError;

/// The file `write_runs` puts in the work dir, preserved as `frames/runs.yml`.
pub(crate) const RUNS_FILE: &str = "runs.yml";

/// Extensions of the downloaded source video and yt-dlp's intermediates. The
/// preserved frames tree keeps every extracted frame, never the video.
const SOURCE_MEDIA_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "m4a", "part"];

/// What became of one run's preserved frames work dirs: how many guards are
/// still live and every preserve failure (copy, `frames.yml` rewrite,
/// `runs.yml` write). The pipeline holds one per run and reads it once every
/// guard is retired; a guard that preserves nothing never touches it, so it
/// stays empty under `PublishPolicy::Vault`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreserveReport(Arc<Mutex<PreserveState>>);

#[derive(Debug, Default)]
struct PreserveState {
    live: usize,
    failures: Vec<String>,
}

impl PreserveReport {
    fn state(&self) -> std::sync::MutexGuard<'_, PreserveState> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn arm(&self) {
        self.state().live += 1;
    }

    fn settle(&self, failure: Option<String>) {
        let mut state = self.state();
        state.live = state.live.saturating_sub(1);
        state.failures.extend(failure);
    }

    /// Record a preserve failure found outside the guard (the `runs.yml` write).
    pub(crate) fn record_failure(&self, failure: String) {
        self.state().failures.push(failure);
    }

    /// Why the run's frames output is not trustworthy: a failure was recorded,
    /// or a guard is still live (its preserve has not run). `None` when every
    /// preserve finished cleanly, or none was asked for.
    pub(crate) fn failure(&self) -> Option<String> {
        let state = self.state();
        log::debug!(
            "PreserveReport::failure: live={} failures={}",
            state.live,
            state.failures.len()
        );
        if !state.failures.is_empty() {
            return Some(format!("frames preserve failed: {}", state.failures.join("; ")));
        }
        (state.live > 0).then(|| format!("frames preserve failed: {} work dir(s) never retired", state.live))
    }
}

/// Owns a YouTube frames work dir (the <=720p mp4 plus every extracted frame)
/// and retires it when dropped: with a preserve destination it first copies
/// the dir there minus the source video, then removes it. Every early return
/// of `try_extract_slides` used to leave the dir behind: content filtering
/// makes most videos text-only, so nearly every ingest leaked one, 40G of
/// /tmp in a day, until the per-user tmpfs quota failed downloads and writes.
/// The slide-note path carries the guard in `SlidePayload` and retires it
/// once `publish_slides` has copied what it embeds, so drop is the one exit.
/// A preserve failure goes to the [`PreserveReport`], never a panic, and the
/// dir is removed regardless.
#[derive(Debug)]
pub(crate) struct WorkDirGuard {
    path: PathBuf,
    preserve_to: Option<PathBuf>,
    report: PreserveReport,
}

impl WorkDirGuard {
    /// `preserve_to` is where the dir is copied before removal (the scratch
    /// root's `frames/`), or `None` to remove it outright. A preserving guard
    /// counts itself live in `report` until it retires.
    pub(crate) fn new(path: PathBuf, preserve_to: Option<PathBuf>, report: PreserveReport) -> Self {
        if preserve_to.is_some() {
            report.arm();
        }
        Self {
            path,
            preserve_to,
            report,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The report a preserving guard settles into; `None` when nothing is
    /// preserved, so `runs.yml` is written exactly when it will be kept.
    pub(crate) fn preserving(&self) -> Option<&PreserveReport> {
        self.preserve_to.as_ref().map(|_| &self.report)
    }

    /// Retire the dir now rather than at scope end: preserve, then remove.
    pub(crate) fn retire(self) {
        log::debug!("retiring frames work dir {}", self.path.display());
    }
}

impl AsRef<Path> for WorkDirGuard {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDirGuard {
    fn drop(&mut self) {
        let Some(dest) = self.preserve_to.as_deref() else {
            remove_work_dir(&self.path);
            return;
        };
        let outcome = preserve_work_dir(&self.path, dest);
        remove_work_dir(&self.path);
        let failure = match outcome {
            Ok(copied) => {
                log::info!(
                    "preserved frames work dir {} -> {} ({copied} files)",
                    self.path.display(),
                    dest.display()
                );
                None
            }
            Err(e) => {
                log::warn!(
                    "could not preserve frames work dir {} -> {}: {e:#}",
                    self.path.display(),
                    dest.display()
                );
                Some(format!("{} -> {}: {e:#}", self.path.display(), dest.display()))
            }
        };
        self.report.settle(failure);
    }
}

/// Copy `path` to `dest`, then make the copied `frames.yml` point inside
/// `dest`: its frame paths are absolute into `path`, which is about to go.
fn preserve_work_dir(path: &Path, dest: &Path) -> Result<usize> {
    let copied = copy_tree(path, dest)?;
    relativize_frames_sidecar(path, dest)?;
    Ok(copied)
}

/// Remove the work dir; a failure is a WARN (a missing dir is fine).
fn remove_work_dir(path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("could not remove frames work dir {}: {e}", path.display());
    }
}

/// Rewrite the preserved `dest/frames.yml` so each frame path is relative to
/// `dest`, as `runs.yml` already is. `extract_frames` records the paths it
/// read back from the /tmp work dir; only the preserved copy is rewritten, so
/// nothing that reads the work dir's own sidecar sees a change. No sidecar
/// (frame extraction never wrote one) is nothing to do; a frame outside the
/// work dir is an error, never a dangling path left in place.
fn relativize_frames_sidecar(work_dir: &Path, dest: &Path) -> Result<()> {
    let sidecar_path = dest.join(crate::youtube::FRAMES_SIDECAR_FILE);
    if !sidecar_path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&sidecar_path).with_context(|| format!("read {}", sidecar_path.display()))?;
    let mut sidecar: crate::youtube::FramesSidecar =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", sidecar_path.display()))?;
    for frame in &mut sidecar.frames {
        if frame.path.is_relative() {
            continue;
        }
        frame.path = frame
            .path
            .strip_prefix(work_dir)
            .with_context(|| {
                format!(
                    "frame {} is outside the work dir {}",
                    frame.path.display(),
                    work_dir.display()
                )
            })?
            .to_path_buf();
    }
    let yaml = serde_yaml::to_string(&sidecar).context("serialize frames.yml")?;
    std::fs::write(&sidecar_path, yaml).with_context(|| format!("write {}", sidecar_path.display()))?;
    log::debug!(
        "relativize_frames_sidecar: {} frames -> {}",
        sidecar.frames.len(),
        sidecar_path.display()
    );
    Ok(())
}

/// Recursively copy the regular files under `src` into `dest`, skipping the
/// source video ([`SOURCE_MEDIA_EXTENSIONS`]) and anything that is neither a
/// file nor a directory (symlinks included). Copy, not rename: /tmp and the
/// scratch root may be on different filesystems. Returns the files copied; a
/// missing `src` copies nothing.
fn copy_tree(src: &Path, dest: &Path) -> Result<usize> {
    log::debug!("copy_tree: {} -> {}", src.display(), dest.display());
    if !src.exists() {
        return Ok(0);
    }
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut copied = 0;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", src.display()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry.file_type().with_context(|| format!("stat {}", from.display()))?;
        if kind.is_dir() {
            copied += copy_tree(&from, &to)?;
        } else if kind.is_file() && !is_source_media(&from) {
            std::fs::copy(&from, &to).with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
            copied += 1;
        } else {
            log::trace!("copy_tree: skipping {}", from.display());
        }
    }
    Ok(copied)
}

fn is_source_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// One content-filter run in `runs.yml`: its window, the best frame that was
/// classified, the verdict, and that frame's OCR. A failed classification or
/// OCR leaves its value `null` and names why.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct RunRecord {
    pub start: f64,
    pub end: f64,
    /// Relative to the work dir, so it resolves under the preserved `frames/`.
    pub best_frame: PathBuf,
    pub category: Option<SlideCategory>,
    pub confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classify_error: Option<String>,
    pub ocr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_error: Option<String>,
}

/// Build one [`RunRecord`] per classified run, OCR'ing every best frame with
/// `ocr` (production passes `ocr::ocr_extract`), kept or dropped. `windows`,
/// `best_frames` and `classes` are positionally aligned, as
/// `classify::apply_filter` takes them.
pub(crate) fn run_records(
    windows: &[Run],
    best_frames: &[PathBuf],
    classes: &[std::result::Result<SlideClass, ClassifyError>],
    work_dir: &Path,
    ocr: fn(&Path, u64) -> Result<String>,
    ocr_timeout_secs: u64,
) -> Vec<RunRecord> {
    log::debug!(
        "run_records: runs={} work_dir={} ocr_timeout={ocr_timeout_secs}s",
        best_frames.len(),
        work_dir.display()
    );
    let ocr_results: Vec<Result<String>> = best_frames
        .par_iter()
        .map(|frame| ocr(frame, ocr_timeout_secs))
        .collect();
    windows
        .iter()
        .zip(best_frames)
        .zip(classes)
        .zip(ocr_results)
        .map(|(((window, frame), class), ocr_result)| {
            let (category, confidence, classify_error) = match class {
                Ok(c) => (Some(c.category), Some(c.confidence), None),
                Err(e) => (None, None, Some(e.to_string())),
            };
            let (ocr, ocr_error) = match ocr_result {
                Ok(text) => (Some(text), None),
                Err(e) => {
                    log::warn!("run OCR failed for {}: {e:#}", frame.display());
                    (None, Some(format!("{e:#}")))
                }
            };
            RunRecord {
                start: window.start,
                end: window.end,
                best_frame: frame.strip_prefix(work_dir).unwrap_or(frame).to_path_buf(),
                category,
                confidence,
                classify_error,
                ocr,
                ocr_error,
            }
        })
        .collect()
}

/// Write `records` as a YAML list to `work_dir/runs.yml`.
pub(crate) fn write_runs(records: &[RunRecord], work_dir: &Path) -> Result<PathBuf> {
    let path = work_dir.join(RUNS_FILE);
    let yaml = serde_yaml::to_string(records).context("serialize runs.yml")?;
    std::fs::write(&path, yaml).with_context(|| format!("write {}", path.display()))?;
    log::debug!("write_runs: {} runs -> {}", records.len(), path.display());
    Ok(path)
}

#[cfg(test)]
mod tests;
