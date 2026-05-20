use crate::config::Config;
use crate::ledger;
use crate::migrate::reclassify_type;
use crate::quality;
use eyre::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum AuditFinding {
    MistypedContent {
        source: String,
        current_type: String,
        expected_type: String,
        note_path: Option<PathBuf>,
    },
    BlockedContent {
        source: String,
        title: String,
        note_path: Option<PathBuf>,
    },
    RawUrlTitle {
        source: String,
        title: String,
        note_path: Option<PathBuf>,
    },
    DuplicateNotes {
        source: String,
        note_paths: Vec<PathBuf>,
    },
    OrphanedReplacement {
        source: String,
        replaced_date: String,
    },
}

impl std::fmt::Display for AuditFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditFinding::MistypedContent {
                source,
                current_type,
                expected_type,
                ..
            } => write!(
                f,
                "[MISTYPE] {source} -> type should be: {expected_type} (currently: {current_type})"
            ),
            AuditFinding::BlockedContent { source, title, .. } => {
                write!(f, "[BLOCKED] {source} -> title: \"{title}\"")
            }
            AuditFinding::RawUrlTitle { source, title, .. } => {
                write!(f, "[RAW-TITLE] {source} -> title is raw URL: \"{title}\"")
            }
            AuditFinding::DuplicateNotes { source, note_paths } => {
                write!(f, "[DUPLICATE] {source} -> {} notes found", note_paths.len())
            }
            AuditFinding::OrphanedReplacement { source, replaced_date } => {
                write!(
                    f,
                    "[ORPHAN-REPLACE] {source} -> marked replaced on {replaced_date} but no replacement ✅ exists"
                )
            }
        }
    }
}

/// Outcome of `borg::audit::run`. Carries the structured findings plus the
/// pre-rendered lines sb prints (header + summary + fix-attempt log).
#[derive(Debug, Default)]
pub struct AuditReport {
    pub findings: Vec<AuditFinding>,
    /// Number of fixes applied (only nonzero when `--fix` was passed).
    pub fixed_count: usize,
    /// Pre-rendered output lines: header, summary section, per-finding details,
    /// fix-mode log entries. sb iterates and prints.
    pub lines: Vec<String>,
}

pub fn run(config: &Config, fix: bool) -> Result<AuditReport> {
    let ledger_path = ledger::ledger_path(config);
    let vault_root = expand_tilde(&config.vault.root_path);
    let mut lines: Vec<String> = Vec::new();

    if !ledger_path.exists() {
        lines.push(format!("No Borg Ledger found at {}", ledger_path.display()));
        return Ok(AuditReport {
            findings: Vec::new(),
            fixed_count: 0,
            lines,
        });
    }

    lines.push(format!("Auditing Borg Ledger: {}", ledger_path.display()));
    lines.push(format!("Vault: {}", vault_root.display()));

    let entries = ledger::parse_completed_entries(&ledger_path)?;
    lines.push(format!("Found {} completed ledger entries to audit\n", entries.len()));

    // Build a map of source URL -> note paths in vault
    let note_index = build_note_index(&vault_root, &config.migration.skip_folders)?;

    let mut findings: Vec<AuditFinding> = Vec::new();

    // Check each completed ledger entry
    for entry in &entries {
        // Skip image entries (not URLs)
        if entry.source.starts_with("[image:") {
            continue;
        }

        // 1. Type misclassification
        let expected_type = reclassify_type(&entry.source);
        if let Some(paths) = note_index.get(&entry.source) {
            for path in paths {
                if let Some(current_type) = read_note_type(path)
                    && current_type != expected_type
                {
                    findings.push(AuditFinding::MistypedContent {
                        source: entry.source.clone(),
                        current_type,
                        expected_type: expected_type.to_string(),
                        note_path: Some(path.clone()),
                    });
                }
            }
        }

        // 2. Blocked content / Raw URL titles. The ledger no longer carries
        //    the human title, so read it from the note's frontmatter via the
        //    note_index. Notes missing from the vault simply skip this check.
        let note_path = note_index.get(&entry.source).and_then(|p| p.first()).cloned();
        if let Some(ref path) = note_path
            && let Some(note_title) = read_note_title(path)
            && let Some(reason) = quality::detect_blocked_content("", &note_title)
        {
            if reason.contains("raw URL") {
                findings.push(AuditFinding::RawUrlTitle {
                    source: entry.source.clone(),
                    title: note_title,
                    note_path,
                });
            } else {
                findings.push(AuditFinding::BlockedContent {
                    source: entry.source.clone(),
                    title: note_title,
                    note_path,
                });
            }
        }
    }

    // 3. Orphaned replacements (🔄 entries with no corresponding ✅)
    {
        let content = std::fs::read_to_string(&ledger_path).context("Failed to read Borg Ledger for orphan check")?;
        let mut replaced_sources: Vec<(String, String)> = Vec::new(); // (source, date)
        let mut completed_sources: std::collections::HashSet<String> = std::collections::HashSet::new();

        for line in content.lines() {
            if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
                continue;
            }
            let cols: Vec<&str> = line.split('|').collect();
            if cols.len() < 8 {
                continue;
            }
            let status = cols[4].trim();
            let source = if cols.len() >= 11 { cols[7].trim() } else { cols[6].trim() };

            if status == "✅" {
                completed_sources.insert(source.to_string());
            } else if status == "🔄" {
                replaced_sources.push((source.to_string(), cols[1].trim().to_string()));
            }
        }

        for (source, date) in &replaced_sources {
            if !completed_sources.contains(source) {
                findings.push(AuditFinding::OrphanedReplacement {
                    source: source.clone(),
                    replaced_date: date.clone(),
                });
            }
        }
    }

    // 4. Duplicate notes (multiple notes with same source URL)
    for (source, paths) in &note_index {
        if paths.len() > 1 {
            findings.push(AuditFinding::DuplicateNotes {
                source: source.clone(),
                note_paths: paths.clone(),
            });
        }
    }

    // Report
    if findings.is_empty() {
        lines.push("No issues found.".to_string());
        return Ok(AuditReport {
            findings,
            fixed_count: 0,
            lines,
        });
    }

    // Categorize
    let mut mistype_count = 0;
    let mut blocked_count = 0;
    let mut raw_title_count = 0;
    let mut duplicate_count = 0;
    let mut orphan_count = 0;

    for finding in &findings {
        match finding {
            AuditFinding::MistypedContent { .. } => mistype_count += 1,
            AuditFinding::BlockedContent { .. } => blocked_count += 1,
            AuditFinding::RawUrlTitle { .. } => raw_title_count += 1,
            AuditFinding::DuplicateNotes { .. } => duplicate_count += 1,
            AuditFinding::OrphanedReplacement { .. } => orphan_count += 1,
        }
    }

    lines.push("Audit Results:".to_string());
    if mistype_count > 0 {
        lines.push(format!("  {mistype_count} misclassified types"));
    }
    if blocked_count > 0 {
        lines.push(format!("  {blocked_count} blocked content saved as completed"));
    }
    if raw_title_count > 0 {
        lines.push(format!("  {raw_title_count} raw URL titles"));
    }
    if duplicate_count > 0 {
        lines.push(format!("  {duplicate_count} duplicate note pairs"));
    }
    if orphan_count > 0 {
        lines.push(format!(
            "  {orphan_count} orphaned replacements (replaced but no new ✅)"
        ));
    }

    lines.push("\nDetails:".to_string());
    for finding in &findings {
        lines.push(format!("  {finding}"));
    }

    // Fix mode: update mistyped notes
    let mut fixed_count = 0;
    if fix {
        let fixable: Vec<&AuditFinding> = findings
            .iter()
            .filter(|f| matches!(f, AuditFinding::MistypedContent { .. }))
            .collect();

        if fixable.is_empty() {
            lines.push("\nNo fixable issues (only type misclassifications can be auto-fixed).".to_string());
        } else {
            lines.push(format!("\nFixing {} misclassified types...", fixable.len()));
            for finding in &fixable {
                if let AuditFinding::MistypedContent {
                    expected_type,
                    note_path: Some(path),
                    ..
                } = finding
                {
                    match fix_note_type(path, expected_type) {
                        Ok(()) => {
                            let rel = path.strip_prefix(&vault_root).unwrap_or(path);
                            lines.push(format!("  Fixed: {} -> type: {expected_type}", rel.display()));
                            fixed_count += 1;
                        }
                        Err(e) => {
                            lines.push(format!("  Error fixing {}: {e:#}", path.display()));
                        }
                    }
                }
            }
        }
    } else {
        let fixable_count = findings
            .iter()
            .filter(|f| matches!(f, AuditFinding::MistypedContent { .. }))
            .count();
        if fixable_count > 0 {
            lines.push(format!(
                "\nRun with --fix to correct {fixable_count} misclassified types."
            ));
        }
    }

    Ok(AuditReport {
        findings,
        fixed_count,
        lines,
    })
}

/// Build an index mapping source URL -> list of note file paths in the vault.
///
/// Parallel over the `.md` file list via rayon. `par_iter().filter_map().collect::<Vec<_>>()`
/// preserves the input-slice order (which `collect_md_files` already sorts), so when we fold
/// the `(source, path)` pairs into the final `HashMap<String, Vec<PathBuf>>` sequentially on
/// the main thread the per-source `Vec<PathBuf>` ordering is identical to the previous
/// sequential implementation.
fn build_note_index(vault_root: &Path, skip_folders: &[String]) -> Result<HashMap<String, Vec<PathBuf>>> {
    use rayon::prelude::*;

    log::debug!(
        "audit::build_note_index: vault_root={} skip_folders={:?}",
        vault_root.display(),
        skip_folders
    );
    let md_files = collect_md_files(vault_root, skip_folders)?;

    let pairs: Vec<(String, PathBuf)> = md_files
        .par_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            let source = extract_frontmatter_field(&content, "source")?;
            Some((source, path.clone()))
        })
        .collect();

    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (source, path) in pairs {
        index.entry(source).or_default().push(path);
    }
    log::debug!("audit::build_note_index: indexed {} source url(s)", index.len());

    Ok(index)
}

/// Read the `type:` field from a note's frontmatter.
fn read_note_type(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_frontmatter_field(&content, "type")
}

/// Read the `title:` field from a note's frontmatter.
fn read_note_title(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    extract_frontmatter_field(&content, "title")
}

/// Extract a simple string field from YAML frontmatter.
fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first.find("\n---")?;
    let fm = &after_first[..end_pos];

    for line in fm.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&format!("{field}:")) {
            let val = rest.trim().trim_matches('"');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Fix the `type:` field in a note's frontmatter.
fn fix_note_type(path: &Path, new_type: &str) -> Result<()> {
    let content = std::fs::read_to_string(path).context("Failed to read note")?;
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        eyre::bail!("No frontmatter found");
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first
        .find("\n---")
        .ok_or_else(|| eyre::eyre!("Unclosed frontmatter"))?;

    let fm = &after_first[..end_pos];
    let body = &after_first[end_pos..];

    // Replace the type field in frontmatter
    let mut new_fm_lines: Vec<String> = Vec::new();
    let mut found_type = false;
    for line in fm.lines() {
        if line.trim().starts_with("type:") {
            new_fm_lines.push(format!("type: {new_type}"));
            found_type = true;
        } else {
            new_fm_lines.push(line.to_string());
        }
    }
    if !found_type {
        // Add type field if not present
        new_fm_lines.push(format!("type: {new_type}"));
    }

    let new_content = format!("---\n{}{}", new_fm_lines.join("\n"), body);
    std::fs::write(path, new_content).context("Failed to write fixed note")?;
    Ok(())
}

fn collect_md_files(root: &Path, skip_folders: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_md_recursive(root, root, skip_folders, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_recursive(current: &Path, root: &Path, skip_folders: &[String], files: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(current).context(format!("Failed to read dir: {}", current.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if skip_folders.iter().any(|s| rel.starts_with(s)) {
                continue;
            }
            collect_md_recursive(&path, root, skip_folders, files)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    Ok(())
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_frontmatter_field_source() {
        let content = "---\ntitle: \"Test\"\nsource: \"https://example.com\"\ntype: article\n---\n\n# Body\n";
        assert_eq!(
            extract_frontmatter_field(content, "source"),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_extract_frontmatter_field_type() {
        let content = "---\ntitle: \"Test\"\ntype: youtube\n---\n\n# Body\n";
        assert_eq!(extract_frontmatter_field(content, "type"), Some("youtube".to_string()));
    }

    #[test]
    fn test_extract_frontmatter_field_missing() {
        let content = "---\ntitle: \"Test\"\n---\n\n# Body\n";
        assert_eq!(extract_frontmatter_field(content, "source"), None);
    }

    #[test]
    fn test_extract_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n";
        assert_eq!(extract_frontmatter_field(content, "type"), None);
    }

    #[test]
    fn test_audit_finding_display() {
        let finding = AuditFinding::MistypedContent {
            source: "https://github.com/owner/repo".to_string(),
            current_type: "article".to_string(),
            expected_type: "github".to_string(),
            note_path: None,
        };
        let display = format!("{finding}");
        assert!(display.contains("[MISTYPE]"));
        assert!(display.contains("github"));
        assert!(display.contains("article"));
    }

    #[test]
    fn test_audit_finding_display_blocked() {
        let finding = AuditFinding::BlockedContent {
            source: "https://example.com".to_string(),
            title: "Just a moment...".to_string(),
            note_path: None,
        };
        let display = format!("{finding}");
        assert!(display.contains("[BLOCKED]"));
        assert!(display.contains("Just a moment"));
    }

    #[test]
    fn test_audit_finding_display_raw_title() {
        let finding = AuditFinding::RawUrlTitle {
            source: "https://example.com".to_string(),
            title: "https://example.com".to_string(),
            note_path: None,
        };
        let display = format!("{finding}");
        assert!(display.contains("[RAW-TITLE]"));
    }

    #[test]
    fn test_audit_finding_display_duplicate() {
        let finding = AuditFinding::DuplicateNotes {
            source: "https://example.com".to_string(),
            note_paths: vec![PathBuf::from("/a.md"), PathBuf::from("/b.md")],
        };
        let display = format!("{finding}");
        assert!(display.contains("[DUPLICATE]"));
        assert!(display.contains("2 notes"));
    }

    #[test]
    fn test_fix_note_type() {
        let dir = std::env::temp_dir().join("obsidian-borg-test-fix-type");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("test-fix.md");
        std::fs::write(
            &path,
            "---\ntitle: \"Test\"\ntype: article\ntags:\n  - test\n---\n\n# Body\n",
        )
        .expect("write");

        fix_note_type(&path, "github").expect("fix");

        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.contains("type: github"));
        assert!(!content.contains("type: article"));
        assert!(content.contains("title: \"Test\""));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_audit_finding_display_orphaned_replacement() {
        let finding = AuditFinding::OrphanedReplacement {
            source: "https://example.com/video".to_string(),
            replaced_date: "2026-03-18".to_string(),
        };
        let display = format!("{finding}");
        assert!(display.contains("[ORPHAN-REPLACE]"));
        assert!(display.contains("2026-03-18"));
        assert!(display.contains("no replacement"));
    }

    /// Phase 3 determinism guard: parallel `build_note_index` must produce the same per-source
    /// `Vec<PathBuf>` ordering as the previous sequential implementation. The contract: notes
    /// that share a source URL appear in the same order they would appear in the sorted
    /// `collect_md_files` output. Build a fixture where two notes point at the same source URL
    /// and verify both the key set and the per-key path order.
    #[test]
    fn build_note_index_per_source_order_matches_sorted_md_files_under_par_iter() {
        // Fixture of 50 notes - the size the design doc specifies. The 50 notes split across
        // three source URLs so each URL's Vec<PathBuf> has many entries, making any par_iter
        // ordering bug obvious. Names use zero-padded indices so sort-order matches numeric
        // order independent of platform locale.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        let shared_url = "https://example.com/shared";
        let other_url = "https://example.com/other";
        let unique_url = "https://example.com/unique";

        let mut expected_shared = Vec::new();
        let mut expected_other = Vec::new();
        for i in 0u32..50 {
            let name = format!("note-{i:03}.md");
            // Spread across three buckets: 25 shared, 24 other, 1 unique.
            let src = if i == 25 {
                unique_url
            } else if i % 2 == 0 {
                shared_url
            } else {
                other_url
            };
            std::fs::write(
                root.join(&name),
                format!("---\ntitle: N{i}\nsource: {src}\n---\nbody\n"),
            )
            .expect("write fixture note");
            if src == shared_url {
                expected_shared.push(name.clone());
            } else if src == other_url {
                expected_other.push(name.clone());
            }
        }

        let index = build_note_index(root, &[]).expect("index");

        let shared = index.get(shared_url).expect("shared key present");
        let shared_names: Vec<String> = shared
            .iter()
            .map(|p| p.file_name().expect("path has filename").to_string_lossy().to_string())
            .collect();
        assert_eq!(
            shared_names, expected_shared,
            "per-source path order for {shared_url} must match sorted collect_md_files order"
        );

        let other = index.get(other_url).expect("other key present");
        let other_names: Vec<String> = other
            .iter()
            .map(|p| p.file_name().expect("path has filename").to_string_lossy().to_string())
            .collect();
        assert_eq!(
            other_names, expected_other,
            "per-source path order for {other_url} must match sorted collect_md_files order"
        );

        let unique = index.get(unique_url).expect("unique key present");
        assert_eq!(unique.len(), 1);
    }
}
