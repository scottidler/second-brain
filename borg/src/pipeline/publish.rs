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
            domain: None,
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
pub(crate) fn find_note_by_source(vault_root: &std::path::Path, source_url: &str) -> Option<PathBuf> {
    let needle = format!("source: \"{source_url}\"");
    find_note_by_source_recursive(vault_root, &needle)
}

pub(crate) fn find_note_by_source_recursive(dir: &std::path::Path, needle: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip system directories that won't contain ingested notes
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "templates" {
                continue;
            }
            if let Some(found) = find_note_by_source_recursive(&path, needle) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|ext| ext == "md") {
            // Quick check: read first 2KB (frontmatter is always at the top)
            if let Ok(file) = std::fs::File::open(&path) {
                use std::io::Read;
                let mut buf = vec![0u8; 2048];
                let mut reader = std::io::BufReader::new(file);
                let n = reader.read(&mut buf).unwrap_or(0);
                let header = String::from_utf8_lossy(&buf[..n]);
                if header.contains(needle) {
                    return Some(path);
                }
            }
        }
    }
    None
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

/// Read cortex-managed fields from frontmatter.
/// Assumes all values are single-line (inline YAML). This holds for all current
/// cortex fields: `cortex-quality-issues` uses inline arrays like `[no-outbound-links]`.
pub(crate) fn read_cortex_fields(path: &std::path::Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut fields = Vec::new();
    let mut in_frontmatter = false;
    for line in content.lines() {
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
        for key in CORTEX_PRESERVE_KEYS {
            if let Some(val) = line.strip_prefix(&format!("{key}:")) {
                fields.push((key.to_string(), val.trim().to_string()));
            }
        }
    }
    fields
}
