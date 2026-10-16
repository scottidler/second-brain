//! Channel backfill (`sb borg channels --backfill [--apply]`): stamp
//! `channel-id`, `channel-handle` and `channel-url` onto YouTube notes that
//! predate persisted channel metadata.
//!
//! Staging holds no channel id, so each candidate costs one
//! `yt-dlp --dump-json`, keyed on that note's OWN `source:` video URL, never
//! on the creator name (two channels can share a display name). The yt-dlp
//! call sits behind [`ChannelFetcher`] so tests use a fake. A key already in
//! the frontmatter is never overwritten, and writes go through the shared
//! atomic note writer. A video that is deleted, private, or otherwise
//! un-fetchable is counted and skipped, never a failure that stops the loop.
//! Dry run (the default) finds candidates from the notes alone and makes no
//! network call.

use async_trait::async_trait;
use eyre::{Context, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::markdown::yaml_scalar;
use crate::pipeline::atomic::write_atomic;
use crate::youtube::{VideoMetadata, extract_video_id, fetch_metadata};

pub const KEY_ID: &str = "channel-id";
pub const KEY_HANDLE: &str = "channel-handle";
pub const KEY_URL: &str = "channel-url";
const CHANNEL_KEYS: [&str; 3] = [KEY_ID, KEY_HANDLE, KEY_URL];

/// Seam over the one yt-dlp call the backfill makes per note.
#[async_trait]
pub trait ChannelFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<VideoMetadata>;
}

/// Production fetcher: `yt-dlp --dump-json` via [`fetch_metadata`].
pub struct YtDlpFetcher {
    pub timeout_secs: u64,
}

#[async_trait]
impl ChannelFetcher for YtDlpFetcher {
    async fn fetch(&self, url: &str) -> Result<VideoMetadata> {
        fetch_metadata(url, self.timeout_secs).await
    }
}

#[derive(Debug, Clone, Default)]
pub struct BackfillReport {
    /// YouTube notes with a video URL in `source:`.
    pub scanned: usize,
    /// Notes with all three `channel-*` keys already present.
    pub already_complete: usize,
    /// Notes written (`apply`) or that would be (dry run).
    pub to_write: Vec<PathBuf>,
    /// `(path, reason)` for videos yt-dlp could not describe (deleted,
    /// private, timed out). Counted and skipped.
    pub unavailable: Vec<(PathBuf, String)>,
    /// `(path, reason)` for notes that could not be read or written.
    pub failed: Vec<(PathBuf, String)>,
}

/// Backfill with the real yt-dlp fetcher.
pub async fn run(config: &Config, apply: bool) -> Result<BackfillReport> {
    let fetcher = YtDlpFetcher {
        timeout_secs: config.pipeline.yt_dlp_timeout_secs,
    };
    run_with(config, &fetcher, apply).await
}

pub async fn run_with(config: &Config, fetcher: &dyn ChannelFetcher, apply: bool) -> Result<BackfillReport> {
    let vault_root = config.vault_root()?;
    log::debug!("channels::run_with: vault_root={} apply={apply}", vault_root.display());
    let mut report = BackfillReport::default();
    for path in collect_md_files(&vault_root, &config.migration.skip_folders)? {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(source) = youtube_source(&content) else {
            continue;
        };
        report.scanned += 1;
        if missing_keys(&content).is_empty() {
            report.already_complete += 1;
            continue;
        }
        if !apply {
            report.to_write.push(path);
            continue;
        }
        let meta = match fetcher.fetch(&source).await {
            Ok(m) => m,
            Err(err) => {
                log::warn!("channels backfill: {} unavailable: {err:#}", path.display());
                report.unavailable.push((path, format!("{err:#}")));
                continue;
            }
        };
        match apply_to_note(&path, &meta) {
            Ok(true) => report.to_write.push(path),
            Ok(false) => report
                .unavailable
                .push((path, "yt-dlp returned no channel metadata".to_string())),
            Err(err) => {
                log::warn!("channels backfill: {} write failed: {err:#}", path.display());
                report.failed.push((path, format!("{err:#}")));
            }
        }
    }
    log::info!(
        "channels backfill: scanned={} complete={} to_write={} unavailable={} failed={} apply={apply}",
        report.scanned,
        report.already_complete,
        report.to_write.len(),
        report.unavailable.len(),
        report.failed.len()
    );
    Ok(report)
}

/// The note's own video URL when it is a `type: youtube` note whose
/// `source:` carries a video id.
fn youtube_source(content: &str) -> Option<String> {
    let (fm, _) = vault::frontmatter::split_raw(content)?;
    if frontmatter_value(fm, "type")? != "youtube" {
        return None;
    }
    let source = frontmatter_value(fm, "source")?;
    extract_video_id(&source)?;
    Some(source)
}

fn frontmatter_value(fm: &str, key: &str) -> Option<String> {
    fm.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        let val = rest.trim().trim_matches(['"', '\'']);
        (!val.is_empty()).then(|| val.to_string())
    })
}

/// The `channel-*` keys with no line in the frontmatter at all. An existing
/// key, whatever its value, counts as present: the backfill never rewrites it.
fn missing_keys(content: &str) -> Vec<&'static str> {
    let Some((fm, _)) = vault::frontmatter::split_raw(content) else {
        return Vec::new();
    };
    CHANNEL_KEYS
        .into_iter()
        .filter(|key| !fm.lines().any(|l| l.starts_with(&format!("{key}:"))))
        .collect()
}

/// Add each missing `channel-*` key the metadata supplies. `Ok(false)` when
/// nothing could be added.
fn apply_to_note(path: &Path, meta: &VideoMetadata) -> Result<bool> {
    let content = std::fs::read_to_string(path).context("read note")?;
    let Some(updated) = insert_missing(&content, meta) else {
        return Ok(false);
    };
    write_atomic(path, updated.as_bytes()).context("atomic write")?;
    Ok(true)
}

/// Pure: `content` with the missing keys appended at the end of the
/// frontmatter, or `None` when there is nothing to add.
fn insert_missing(content: &str, meta: &VideoMetadata) -> Option<String> {
    let missing = missing_keys(content);
    let values = [
        (KEY_ID, &meta.channel_id),
        (KEY_HANDLE, &meta.channel_handle),
        (KEY_URL, &meta.channel_url),
    ];
    let lines: Vec<String> = values
        .iter()
        .filter(|(key, _)| missing.contains(key))
        .filter_map(|(key, value)| value.as_ref().map(|v| format!("{key}: {}", yaml_scalar(v))))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let (fm, _) = vault::frontmatter::split_raw(content)?;
    // `fm` is a subslice of `content`, so its end offset is the insertion point.
    let fm_end = fm.as_ptr() as usize - content.as_ptr() as usize + fm.len();
    let mut out = String::with_capacity(content.len() + 128);
    out.push_str(&content[..fm_end]);
    if !fm.ends_with('\n') {
        out.push('\n');
    }
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    // `split_raw` drops the newline before the closing fence; restore it only
    // when the frontmatter already ended in one.
    let rest = &content[fm_end..];
    out.push_str(rest.strip_prefix('\n').unwrap_or(rest));
    Some(out)
}

fn collect_md_files(root: &Path, skip_folders: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_md_recursive(root, root, skip_folders, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_recursive(current: &Path, root: &Path, skip_folders: &[String], files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(current).with_context(|| format!("read_dir {}", current.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if skip_folders.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            collect_md_recursive(&path, root, skip_folders, files)?;
        } else if vault::note::is_note_file(&path) {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
