use super::*;

/// Build an obsidian://open deep link from the note path.
///
/// Emits `obsidian://open?file=<stem>` with NO `vault=` parameter. The link is
/// generated once by borg but tapped on multiple devices whose vaults have
/// DIFFERENT names (desktop "obsidian", phone "obsidian-remote"); a hardcoded
/// `vault=obsidian` matched at most one, so on the phone Obsidian opened but
/// could not resolve the named vault to navigate (confirmed on Android 2026-07-04:
/// `vault=obsidian` never navigated in the `obsidian-remote` vault, while the
/// vault-less form did). Omitting `vault` opens the file in each device's CURRENT
/// vault, which is correct everywhere.
///
/// The bare filename stem (no extension, no directory) resolves vault-wide by
/// name, so the link is location-independent and survives cortex's inbox/ ->
/// notes/ promotion. Precondition: the stem is unique across the vault; when two
/// notes share a stem, `open` navigates to whichever one Obsidian's resolver picks.
pub(crate) fn build_obsidian_url(note_path: &str) -> Option<String> {
    let path = std::path::Path::new(note_path);
    let stem = path.file_stem()?.to_str()?;
    let encoded_file = urlencoding::encode(stem);
    Some(format!("obsidian://open?file={encoded_file}"))
}

/// Compute the vault-relative path for a note, for use in the ledger Path column.
/// Returns something like "notes/some-title.md".
pub(crate) fn extract_filename(note_path: &std::path::Path) -> Option<String> {
    note_path.file_name().map(|f| f.to_string_lossy().to_string())
}

/// Finalize a published note: stamp the success ledger row (timezone-aware
/// date/time), build the obsidian deep-link, and assemble the `Completed`
/// `IngestResult`. This is the shared epilogue every type handler runs after
/// `write_atomic` lands the note in the vault — the per-handler parts are the
/// `source` descriptor, `title`, `tags`, and `degraded` flag.
pub(crate) fn publish_note(
    config: &Config,
    note_path: &Path,
    method: IngestMethod,
    source: String,
    title: String,
    tags: Vec<String>,
    trace_id: &str,
    degraded: bool,
) -> Result<IngestResult> {
    let tz = config.frontmatter.timezone_tz();
    let now = chrono::Utc::now().with_timezone(&tz);

    let ledger_file = ledger::ledger_path()?;
    ledger::append_entry(
        &ledger_file,
        &LedgerEntry {
            date: now.format("%Y-%m-%d").to_string(),
            time: now.format("%H:%M").to_string(),
            method,
            filename: extract_filename(note_path),
            source,
            trace_id: Some(trace_id.to_string()),
        },
    )?;

    let obsidian_url = build_obsidian_url(&note_path.to_string_lossy());

    Ok(IngestResult {
        status: IngestStatus::Completed,
        note_path: Some(note_path.to_string_lossy().to_string()),
        title: Some(title),
        tags,
        elapsed_secs: None,
        method: Some(method),
        canonical_url: None,
        trace_id: None,
        obsidian_url,
        failure_stage: None,
        degraded,
    })
}

/// Expand a vault root path (handling ~/) to an absolute PathBuf.
pub fn expand_vault_root(path: &str) -> PathBuf {
    expand_tilde(path)
}

/// Scan the vault for a note whose `source:` frontmatter matches the given URL.
/// Returns the path to the note file if found. This is more reliable than the
/// ledger's stored path because cortex may have moved the file after ingestion.
/// The vault note whose frontmatter `source:` is `source_url`, written quoted or
/// bare. Matching only the quoted form missed the 2,073 notes that store it bare,
/// so a reingest of one published a `-2` copy beside it.
pub(crate) fn find_note_by_source(vault_root: &std::path::Path, source_url: &str) -> Option<PathBuf> {
    find_note_by_source_recursive(vault_root, source_url)
}

pub(crate) fn find_note_by_source_recursive(dir: &std::path::Path, source_url: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip system directories that won't contain ingested notes
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "templates" {
                continue;
            }
            if let Some(found) = find_note_by_source_recursive(&path, source_url) {
                return Some(found);
            }
        } else if vault::note::is_note_file(&path) && read_note_source(&path).as_deref() == Some(source_url) {
            return Some(path);
        }
    }
    None
}

/// The note a reingest should replace when the by-source scan found nothing:
/// the ledger's filename under `notes/` or `inbox/`.
///
/// Titles are not unique across sources, so a file at that name may belong to
/// a different source. When the file's own `source:` is another URL the
/// ledger also records under this filename, it is that source's note and is
/// skipped; replacing it would silently destroy it. A file whose source is
/// unknown to the ledger (a re-canonicalized URL, a legacy note) still counts
/// as this source's note.
pub(crate) fn find_note_by_ledger_filename(
    vault_root: &Path,
    filename: &str,
    canonical: &str,
    ledger_file: &Path,
) -> Result<Option<PathBuf>> {
    if filename == "-" {
        return Ok(None);
    }
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    let other_sources: std::collections::HashSet<String> = ledger::parse_completed_entries(ledger_file)?
        .into_iter()
        .filter(|row| row.slug == stem && row.source != canonical)
        .map(|row| row.source)
        .collect();
    for dir in [vault_root.join("notes"), vault_root.join("inbox")] {
        let candidate = dir.join(filename);
        if !candidate.exists() {
            continue;
        }
        match read_note_source(&candidate) {
            Some(source) if other_sources.contains(&source) => {
                log::info!(
                    "{} belongs to another source ({source}); not the note for {canonical}",
                    candidate.display()
                );
            }
            _ => return Ok(Some(candidate)),
        }
    }
    Ok(None)
}

/// Read the `source:` field from a note's frontmatter, quoted or bare. Only the
/// frontmatter block counts: a `source:` line in the body is not the note's.
fn read_note_source(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = vec![0u8; 4096];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    let header = String::from_utf8_lossy(&buf[..n]);
    let mut lines = header.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    lines
        .take_while(|line| line.trim_end() != "---")
        .find_map(|line| line.strip_prefix("source:"))
        .map(|value| value.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
}

/// Read the `date:` field from a note's frontmatter.
pub(crate) fn read_note_date(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if let Some(date) = line.strip_prefix("date:") {
            return Some(date.trim().to_string());
        }
    }
    None
}

/// A preserved frontmatter value. `tags` arrives as a list in either on-disk
/// form; everything else on `CORTEX_PRESERVE_KEYS` is a scalar, as is every
/// `markdown::BORG_PRESERVE_KEYS` entry (decoded, so a block scalar arrives as
/// its multiline text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Scalar(String),
    List(Vec<String>),
}

/// Strip the surrounding quotes YAML allows on a scalar or a list item. Kept
/// deliberately narrow: this is a frontmatter line, not arbitrary YAML, and a
/// full parse here would reject notes the rest of the pipeline tolerates.
fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    for quote in ['"', '\''] {
        if trimmed.len() >= 2 && trimmed.starts_with(quote) && trimmed.ends_with(quote) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

/// Split an inline `[a, b]` list body into its items.
fn split_inline_list(body: &str) -> Vec<String> {
    body.split(',').map(unquote).filter(|item| !item.is_empty()).collect()
}

/// Decode one borg-owned frontmatter entry: the `key:` line plus every
/// continuation line under it (indented or blank), parsed as YAML. A value
/// borg wrote through `markdown::yaml_scalar` may be a `|-` block scalar or a
/// multi-line quoted string, which a single-line read would truncate.
fn decode_borg_scalar(key: &str, entry: &str) -> eyre::Result<String> {
    let parsed: serde_yaml::Mapping =
        serde_yaml::from_str(entry).map_err(|e| eyre::eyre!("decode preserved `{key}` as YAML: {e}"))?;
    match parsed.get(key) {
        Some(serde_yaml::Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(eyre::eyre!("preserved `{key}` is not a string scalar: {other:?}")),
        None => Err(eyre::eyre!("preserved `{key}` vanished from its own entry")),
    }
}

/// Read every preserved frontmatter field: cortex-owned
/// (`CORTEX_PRESERVE_KEYS`, list-aware) and borg-owned
/// (`markdown::BORG_PRESERVE_KEYS`, YAML-decoded scalars).
///
/// Fails CLOSED. The URL reingest path used to swallow a read error and return
/// an empty set, which published a note that had silently lost its preserved
/// fields; the session path has always failed closed. Both now agree.
pub(crate) fn read_preserved_fields(path: &std::path::Path) -> eyre::Result<Vec<(String, FieldValue)>> {
    use eyre::Context;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read preserved frontmatter fields from {}", path.display()))?;

    let mut fields = Vec::new();
    let mut in_frontmatter = false;
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if !in_frontmatter {
            continue;
        }
        if let Some(key) = markdown::BORG_PRESERVE_KEYS
            .iter()
            .find(|k| line.starts_with(&format!("{k}:")))
        {
            let mut entry = line.to_string();
            while let Some(next) = lines.peek() {
                if !(next.trim().is_empty() || next.starts_with([' ', '\t'])) {
                    break;
                }
                entry.push('\n');
                entry.push_str(next);
                lines.next();
            }
            let value = decode_borg_scalar(key, &entry).with_context(|| format!("in {}", path.display()))?;
            // The ingest-time boilerplate rule applies here too, so a
            // pre-filter `Source: <site>` note is not carried forward.
            let value = if *key == markdown::CAPTURE_NOTE_KEY {
                router::strip_share_boilerplate(&value)
            } else {
                Some(value)
            };
            match value {
                Some(value) => fields.push((key.to_string(), FieldValue::Scalar(value))),
                None => log::debug!("read_preserved_fields: {key} was share boilerplate only; not preserved"),
            }
            continue;
        }
        let Some(key) = CORTEX_PRESERVE_KEYS.iter().find(|k| line.starts_with(&format!("{k}:"))) else {
            continue;
        };
        let rest = line[key.len() + 1..].trim();

        // Inline list: `key: [a, b]`.
        if let Some(body) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            fields.push((key.to_string(), FieldValue::List(split_inline_list(body))));
            continue;
        }
        // Block list: a bare `key:` followed by `- item` lines.
        if rest.is_empty() {
            let mut items = Vec::new();
            while let Some(next) = lines.peek() {
                let Some(item) = next.trim().strip_prefix("- ") else {
                    break;
                };
                items.push(unquote(item));
                lines.next();
            }
            fields.push((key.to_string(), FieldValue::List(items)));
            continue;
        }
        fields.push((key.to_string(), FieldValue::Scalar(unquote(rest))));
    }
    Ok(fields)
}

/// Why the final publish step failed. Every variant is a `publish-failed`
/// trace; the type keeps the archive seam's fail-closed failure distinct from
/// the note write's in logs and the receipt reason.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The durable source archive could not be written, so the vault note was
    /// not written either (2026-10-14-note-sources-intent-tools-channels).
    #[error("source archive write failed: {0}")]
    ArchiveWrite(#[from] crate::archive::ArchiveError),
    /// The atomic note write itself failed.
    #[error("failed to atomically publish note: {0:#}")]
    NoteWrite(eyre::Report),
}

impl PublishError {
    pub fn failure_stage(&self) -> FailureStage {
        match self {
            Self::ArchiveWrite(_) | Self::NoteWrite(_) => FailureStage::PublishFailed,
        }
    }
}

/// The URL publish seam: write the trace's source archive, then the note. The
/// archive comes first and fails closed, so a trace whose source could not be
/// made durable never lands a note that points at a source about to expire.
pub(crate) fn archive_then_publish(
    archive: &crate::archive::SourceArchive<'_>,
    desired_path: &Path,
    bytes: &[u8],
    replaces: Option<&Path>,
    vault_dirs: &[PathBuf],
) -> std::result::Result<PathBuf, PublishError> {
    log::debug!(
        "archive_then_publish: trace={} desired={} replaces={replaces:?}",
        archive.trace_id,
        desired_path.display()
    );
    archive.write()?;
    atomic::publish_unique(desired_path, bytes, replaces, vault_dirs).map_err(PublishError::NoteWrite)
}

/// Everything the URL note writer needs after the note is rendered. The
/// caller has already planned the figures and rendered the note with their
/// `images:` and `## Images`, so the size gate sees the full note.
pub(crate) struct UrlPublish<'a> {
    pub archive: &'a crate::archive::SourceArchive<'a>,
    pub desired_path: &'a Path,
    pub note_bytes: &'a [u8],
    pub replaces: Option<&'a Path>,
    pub vault_dirs: &'a [PathBuf],
    pub vault_root: &'a Path,
    pub max_note_bytes: usize,
    pub figures: &'a [figures::PlannedFigure],
    /// A candidate that passed the prefilter failed, or planning dropped one.
    pub image_stage_degraded: bool,
    /// What the note being replaced owns; empty for a fresh ingest.
    pub old_owned: &'a crate::slides::cleanup::Owned,
    /// The slide paths the new note owns (`slides:` is YouTube's own key).
    pub new_slides: &'a [String],
}

/// Why the URL note writer refused or failed. Each variant maps to the
/// terminal failure stage the receipt records.
#[derive(Debug, thiserror::Error)]
pub enum UrlPublishError {
    /// The composed note is over `pipeline.max-note-bytes`; nothing was written.
    #[error("{0}")]
    SizeGate(String),
    /// This run's image stage degraded and it keeps fewer images than the old
    /// note owns: the old note and its files stay untouched rather than be
    /// replaced by one with fewer figures.
    #[error(
        "image stage degraded and keeps {keeps} image(s), fewer than the {owned} the existing note owns; keeping the existing note and its files"
    )]
    DegradedReingest { owned: usize, keeps: usize },
    /// A figure file could not be written; the ones this run created are removed.
    #[error("failed to store figures: {0:#}")]
    Store(eyre::Report),
    #[error(transparent)]
    Publish(#[from] PublishError),
}

impl UrlPublishError {
    pub fn failure_stage(&self) -> FailureStage {
        match self {
            Self::SizeGate(_) => FailureStage::QualityBlocked,
            Self::DegradedReingest { .. } => FailureStage::FetchFailed,
            Self::Store(_) => FailureStage::PublishFailed,
            Self::Publish(e) => e.failure_stage(),
        }
    }
}

/// The URL note writer. The order is the contract:
///
/// 1. refuse a degraded reingest that would keep fewer images than the old
///    note owns (a degraded run that keeps at least as many publishes, its
///    receipt still degraded);
/// 2. size-gate the fully rendered note, so a fail writes no asset;
/// 3. [`figures::store`] the figure files (no-clobber);
/// 4. [`archive_then_publish`]; if it fails, only the files step 3 created
///    are removed (a reused path is never touched);
/// 5. only after the note is durable, remove the files the old note owned
///    that the new one no longer does (best-effort, logged).
///
/// Returns the published path and the orphans that were targeted.
pub(crate) fn publish_url_note(spec: &UrlPublish<'_>) -> std::result::Result<(PathBuf, Vec<String>), UrlPublishError> {
    let trace_id = spec.archive.trace_id;
    log::debug!(
        "publish_url_note: trace={trace_id} figures={} old_images={} degraded={}",
        spec.figures.len(),
        spec.old_owned.images.len(),
        spec.image_stage_degraded
    );
    let owned = spec.old_owned.images.len();
    let keeps = spec.figures.len();
    if spec.image_stage_degraded && owned > 0 && keeps < owned {
        return Err(UrlPublishError::DegradedReingest { owned, keeps });
    }
    if let Some(reason) = note_size_gate(spec.note_bytes.len(), spec.max_note_bytes) {
        return Err(UrlPublishError::SizeGate(reason));
    }
    let created = figures::store(spec.vault_root, spec.figures).map_err(UrlPublishError::Store)?;
    let note_path = match archive_then_publish(
        spec.archive,
        spec.desired_path,
        spec.note_bytes,
        spec.replaces,
        spec.vault_dirs,
    ) {
        Ok(path) => path,
        Err(e) => {
            if let Err(cleanup) = crate::rkvr::remove(&created) {
                log::warn!(
                    "[{trace_id}] could not remove {} figure(s) after a failed publish: {cleanup:#}",
                    created.len()
                );
            }
            return Err(e.into());
        }
    };
    let new_owned = crate::slides::cleanup::Owned {
        slides: spec.new_slides.to_vec(),
        images: spec.figures.iter().map(|f| f.vault_path.clone()).collect(),
    };
    let orphans = crate::slides::cleanup::compute_all_orphans(spec.old_owned, &new_owned);
    if !orphans.is_empty() {
        let abs = crate::slides::cleanup::resolve_existing(spec.vault_root, &orphans);
        match crate::slides::cleanup::rkvr_remove(&abs) {
            Ok(()) => log::info!("[{trace_id}] archived {} orphan file(s)", orphans.len()),
            Err(e) => log::warn!("[{trace_id}] orphan cleanup failed: {e:#}"),
        }
    }
    Ok((note_path, orphans))
}

/// The terminal `IngestResult` for a failed URL publish, its stage taken from
/// the typed error.
pub(crate) fn url_publish_failure_result(
    err: &UrlPublishError,
    method: IngestMethod,
    canonical: &str,
    trace_id: &str,
) -> IngestResult {
    IngestResult {
        status: IngestStatus::Failed {
            reason: err.to_string(),
        },
        method: Some(method),
        canonical_url: Some(canonical.to_string()),
        trace_id: Some(trace_id.to_string()),
        failure_stage: Some(err.failure_stage()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests;
