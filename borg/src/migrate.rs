use crate::config::Config;
use crate::hygiene;
use crate::ledger::{self, LedgerEntry, LedgerStatus};
use crate::types::IngestMethod;
use eyre::{Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Per-note migration outcome produced by the parallel phase. The sequential drain afterward
/// uses these to update `changed_count`, emit deterministic per-file `println!`s, and
/// accumulate `ledger_entries` for the post-loop seeding step.
struct MigrateOutcome {
    rel_path: String,
    ledger_entry: Option<LedgerEntry>,
}

/// Apply every per-path transform to a single note. Pure CPU + per-file I/O; safe to invoke
/// from a rayon worker. Returns `Ok(None)` when the note does not change (no frontmatter,
/// YAML parse failure, or no applicable transform).
fn migrate_one_note(path: &Path, vault_root: &Path, apply: bool, config: &Config) -> Result<Option<MigrateOutcome>> {
    let migration = &config.migration;
    let content = std::fs::read_to_string(path).context("Failed to read file")?;
    let Some((frontmatter, body)) = split_frontmatter(&content) else {
        return Ok(None);
    };

    let mut fm: HashMap<String, serde_yaml::Value> = match serde_yaml::from_str(&frontmatter) {
        Ok(map) => map,
        Err(_) => return Ok(None),
    };

    let mut changed = false;

    // 1. Field renames
    for (old_name, new_name) in &migration.field_renames {
        if fm.contains_key(old_name)
            && !fm.contains_key(new_name)
            && let Some(val) = fm.remove(old_name)
        {
            fm.insert(new_name.clone(), val);
            changed = true;
        }
    }

    // 2. Value renames
    for (field, renames) in &migration.value_renames {
        if let Some(val) = fm.get(field).and_then(|v| v.as_str()).map(|s| s.to_string())
            && let Some(new_val) = renames.get(&val)
        {
            fm.insert(field.clone(), serde_yaml::Value::String(new_val.clone()));
            changed = true;
        }
    }

    // 3. Field transforms
    for (field, transform) in &migration.field_transforms {
        if let Some(val) = fm.get(field) {
            match transform.as_str() {
                "canonicalize" => {
                    if let Some(url_str) = val.as_str() {
                        match hygiene::normalize_url(url_str, &config.canonicalization.rules) {
                            Ok(canonical) if canonical != url_str => {
                                fm.insert(field.clone(), serde_yaml::Value::String(canonical));
                                changed = true;
                            }
                            _ => {}
                        }
                    }
                }
                "reclassify" => {
                    if let Some(type_str) = val.as_str() {
                        let needs_reclassify = type_str == "link" || type_str == "article";
                        if needs_reclassify {
                            let new_type = if let Some(source) = fm.get("source").and_then(|v| v.as_str()) {
                                reclassify_type(source)
                            } else {
                                "article"
                            };
                            if new_type != type_str {
                                fm.insert(field.clone(), serde_yaml::Value::String(new_type.to_string()));
                                changed = true;
                            }
                        }
                    }
                }
                "normalize" => {
                    // Normalize tags: inline "#tag, #tag" → list, strip #
                    if let Some(tag_str) = val.as_str() {
                        let tags: Vec<serde_yaml::Value> = tag_str
                            .split(',')
                            .map(|t| t.trim().trim_start_matches('#').trim())
                            .filter(|t| !t.is_empty())
                            .map(|t| serde_yaml::Value::String(hygiene::sanitize_tag(t)))
                            .collect();
                        if !tags.is_empty() {
                            fm.insert(field.clone(), serde_yaml::Value::Sequence(tags));
                            changed = true;
                        }
                    }
                }
                _ => {
                    log::warn!("Unknown transform: {transform}");
                }
            }
        }
    }

    // 4. Title fallback
    if migration.title_fallback && !fm.contains_key("title") {
        let title = extract_title_from_body(&body)
            .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        if !title.is_empty() {
            fm.insert("title".to_string(), serde_yaml::Value::String(title));
            changed = true;
        }
    }

    if !changed {
        return Ok(None);
    }

    let rel_path = path.strip_prefix(vault_root).unwrap_or(path).display().to_string();

    if apply {
        let new_content = render_frontmatter(&fm, &body);
        std::fs::write(path, new_content).context("Failed to write migrated file")?;
    }

    let ledger_entry = if migration.seed_borg_log
        && let Some(source) = fm.get("source").and_then(|v| v.as_str())
    {
        let date = fm.get("date").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        let rel_note_path = path.strip_prefix(vault_root).ok().map(|p| p.display().to_string());
        Some(LedgerEntry {
            date,
            time: "00:00".to_string(),
            method: IngestMethod::Cli.into(),
            status: LedgerStatus::Completed,
            filename: rel_note_path.and_then(|p| p.rsplit('/').next().map(|s| s.to_string())),
            source: source.to_string(),
            domain: path
                .parent()
                .and_then(|p| p.strip_prefix(vault_root).ok())
                .map(|p| p.display().to_string()),
            trace_id: None,
        })
    } else {
        None
    };

    Ok(Some(MigrateOutcome { rel_path, ledger_entry }))
}

pub async fn run_migrate(config: &Config, apply: bool) -> Result<()> {
    let migration = &config.migration;
    let vault_root = expand_tilde(&config.vault.root_path);

    if !vault_root.exists() {
        eyre::bail!("Vault root does not exist: {}", vault_root.display());
    }

    let mode = if apply { "APPLY" } else { "DRY-RUN" };
    println!("Migration mode: {mode}");
    println!("Vault: {}", vault_root.display());

    let md_files = collect_md_files(&vault_root, &migration.skip_folders)?;
    println!("Found {} markdown files to check", md_files.len());

    // Parallel phase: read + transform + (conditional) write + (conditional) build LedgerEntry.
    // Per-file independent; std::fs::write is the only side-effect and targets distinct paths.
    // collect::<Result<Vec<_>>>() preserves input-slice order so the sequential drain below
    // produces console output and ledger_entries in the same order as the previous loop.
    let outcomes: Vec<Option<MigrateOutcome>> = md_files
        .par_iter()
        .map(|path| migrate_one_note(path, &vault_root, apply, config))
        .collect::<Result<Vec<_>>>()?;

    let mut changed_count = 0usize;
    let mut ledger_entries: Vec<LedgerEntry> = Vec::new();
    for outcome in outcomes.into_iter().flatten() {
        changed_count += 1;
        println!("  {mode}: {}", outcome.rel_path);
        if let Some(entry) = outcome.ledger_entry {
            ledger_entries.push(entry);
        }
    }

    // Seed Borg Ledger
    if migration.seed_borg_log && apply && !ledger_entries.is_empty() {
        let log_path = ledger::ledger_path(config);
        println!("Seeding Borg Ledger with {} entries...", ledger_entries.len());
        for entry in &ledger_entries {
            // Skip if already in log
            if ledger::check_duplicate(&log_path, &entry.source)?.is_none() {
                ledger::append_entry(&log_path, entry)?;
            }
        }
        println!("Borg Ledger seeded.");
    }

    println!("\n{mode} complete: {changed_count} files would be changed");
    if !apply && changed_count > 0 {
        println!("Run with --apply to write changes.");
    }

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

fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after_first = &trimmed[3..];
    let end_pos = after_first.find("\n---")?;
    let fm = after_first[..end_pos].trim().to_string();
    let body = after_first[end_pos + 4..].to_string();
    Some((fm, body))
}

fn render_frontmatter(fm: &HashMap<String, serde_yaml::Value>, body: &str) -> String {
    // Render with controlled field ordering
    let order = [
        "title",
        "date",
        "day",
        "time",
        "source",
        "type",
        "method",
        "tags",
        "uploader",
        "duration_min",
        "author",
    ];

    let mut lines = vec!["---".to_string()];

    // Render known fields in order
    for key in &order {
        if let Some(val) = fm.get(*key) {
            render_yaml_field(&mut lines, key, val);
        }
    }

    // Render any remaining fields not in the order list
    let mut remaining: Vec<_> = fm.keys().filter(|k| !order.contains(&k.as_str())).collect();
    remaining.sort();
    for key in remaining {
        if let Some(val) = fm.get(key) {
            render_yaml_field(&mut lines, key, val);
        }
    }

    lines.push("---".to_string());

    format!("{}\n{}", lines.join("\n"), body)
}

fn render_yaml_field(lines: &mut Vec<String>, key: &str, val: &serde_yaml::Value) {
    match val {
        serde_yaml::Value::Sequence(seq) => {
            lines.push(format!("{key}:"));
            for item in seq {
                if let Some(s) = item.as_str() {
                    lines.push(format!("  - {s}"));
                }
            }
        }
        serde_yaml::Value::String(s) => {
            if key == "date" || key == "day" || key == "type" || key == "method" {
                lines.push(format!("{key}: {s}"));
            } else {
                lines.push(format!("{key}: \"{s}\""));
            }
        }
        serde_yaml::Value::Number(n) => {
            lines.push(format!("{key}: {n}"));
        }
        serde_yaml::Value::Bool(b) => {
            lines.push(format!("{key}: {b}"));
        }
        _ => {
            if let Ok(s) = serde_yaml::to_string(val) {
                lines.push(format!("{key}: {}", s.trim()));
            }
        }
    }
}

/// Scan every markdown file under the vault root and re-ingest any note
/// whose body matches the failed-fetch signature (block-page paraphrase).
/// This is the migration path for notes that predate the staged pipeline -
/// the 28 XDA-style notes identified in the 2026-04-19 audit.
pub async fn run_reingest_failed(config: &Config, dry_run: bool) -> Result<()> {
    let vault_root = expand_tilde(&config.vault.root_path);
    if !vault_root.exists() {
        eyre::bail!("Vault root does not exist: {}", vault_root.display());
    }
    let md_files = collect_md_files(&vault_root, &config.migration.skip_folders)?;

    // Walk every markdown file in parallel; for each, read + split frontmatter + check the body
    // for the failed-fetch signature, and (on match) extract the source URL. Pure read-only I/O
    // and CPU work; output is order-preserving thanks to rayon's collect, so the eventual HTTP
    // reingest sequence below sees notes in the same order the previous sequential loop did.
    let matches: Vec<(PathBuf, String)> = md_files
        .par_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(path).ok()?;
            let (frontmatter, body) = split_frontmatter(&content)?;
            if !body_has_failed_fetch_signature(&body) {
                return None;
            }
            let fm: HashMap<String, serde_yaml::Value> = serde_yaml::from_str(&frontmatter).ok()?;
            match fm.get("source").and_then(|v| v.as_str()).map(String::from) {
                Some(source) => Some((path.clone(), source)),
                None => {
                    log::warn!(
                        "reingest-failed: {} matches failed-fetch but has no source",
                        path.display()
                    );
                    None
                }
            }
        })
        .collect();

    if matches.is_empty() {
        println!("reingest-failed: no failed-fetch notes found");
        return Ok(());
    }
    let mode = if dry_run { "[dry-run] " } else { "" };
    println!("{mode}reingest-failed: {} matching note(s)", matches.len());
    for (path, source) in &matches {
        let rel = path.strip_prefix(&vault_root).unwrap_or(path.as_path());
        println!("  {}  <- {}", rel.display(), source);
    }
    if dry_run {
        return Ok(());
    }

    // Reingest via the daemon's /ingest endpoint so the request flows through
    // Stage-0 (Gate-0) → fetch chain (Jina → fabric-u → browser-UA) → Gate-1
    // → Stage-2 → Gate-2 → publish, preserving cortex-owned frontmatter.
    let host = &config.hotkey.host;
    let port = config.hotkey.port;
    let endpoint = format!("http://{host}:{port}/ingest");
    let client = reqwest::Client::new();
    for (path, source) in &matches {
        println!("  -> {}", source);
        let body = serde_json::json!({
            "url": source,
            "tags": [],
            "force": true,
            "method": "cli",
        });
        match client.post(&endpoint).json(&body).send().await {
            Ok(response) => match response.json::<crate::types::IngestResult>().await {
                Ok(result) => match &result.status {
                    crate::types::IngestStatus::Completed => {
                        println!("     ok: {}", result.title.as_deref().unwrap_or("(no title)"));
                    }
                    crate::types::IngestStatus::Duplicate { .. } => {
                        println!("     duplicate (unchanged)");
                    }
                    crate::types::IngestStatus::Failed { reason } => {
                        println!("     failed: {reason}");
                    }
                    crate::types::IngestStatus::Queued => {
                        println!("     queued");
                    }
                },
                Err(e) => {
                    eprintln!("     {}: response parse error: {e}", path.display());
                }
            },
            Err(e) => {
                if e.is_connect() {
                    eyre::bail!("cannot reach obsidian-borg at http://{host}:{port} - is the daemon running?");
                }
                eprintln!("     {}: HTTP error: {e}", path.display());
            }
        }
    }
    Ok(())
}

/// Detect the failed-fetch signature in a note body. Duplicates the
/// cortex::quality patterns by intent (keeping cortex and borg in sync).
fn body_has_failed_fetch_signature(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "only an error message",
        "no actual content",
        "error message indicating",
        "content inaccessible",
        "access to the website is blocked",
        "anonymous access to domain",
    ];
    PATTERNS.iter().any(|p| lower.contains(p))
}

/// Classify a source URL into the correct content type string.
/// Used by both migrate reclassify and audit.
pub fn reclassify_type(source: &str) -> &'static str {
    use std::sync::LazyLock;

    static YOUTUBE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?:youtube\.com/watch|youtu\.be/|youtube\.com/shorts/)").expect("valid regex")
    });
    static GITHUB_REPO_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^https?://github\.com/[^/]+/[^/]+/?(\?[^ ]*)?$").expect("valid regex"));
    static X_STATUS_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^https?://x\.com/[^/]+/status/\d+").expect("valid regex"));
    static REDDIT_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^https?://(?:www\.)?reddit\.com/r/[^/]+/comments/").expect("valid regex"));

    if YOUTUBE_RE.is_match(source) {
        "youtube"
    } else if GITHUB_REPO_RE.is_match(source) {
        "github"
    } else if X_STATUS_RE.is_match(source) {
        "social"
    } else if REDDIT_RE.is_match(source) {
        "reddit"
    } else {
        "article"
    }
}

fn extract_title_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
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
    fn test_split_frontmatter_valid() {
        let content = "---\ntitle: Test\ntype: link\n---\n\n# Body\n";
        let (fm, body) = split_frontmatter(content).expect("should split");
        assert!(fm.contains("title: Test"));
        assert!(body.contains("# Body"));
    }

    #[test]
    fn test_split_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nSome text.\n";
        assert!(split_frontmatter(content).is_none());
    }

    #[test]
    fn test_split_frontmatter_unclosed() {
        let content = "---\ntitle: Test\nno closing delimiter\n";
        assert!(split_frontmatter(content).is_none());
    }

    #[test]
    fn test_extract_title_from_body() {
        let body = "\n\n# My Title\n\nSome content.";
        assert_eq!(extract_title_from_body(body), Some("My Title".to_string()));
    }

    #[test]
    fn test_extract_title_from_body_none() {
        let body = "\n\nSome content without heading.";
        assert_eq!(extract_title_from_body(body), None);
    }

    #[test]
    fn test_render_frontmatter_ordering() {
        let mut fm = HashMap::new();
        fm.insert("type".to_string(), serde_yaml::Value::String("article".to_string()));
        fm.insert("title".to_string(), serde_yaml::Value::String("Test".to_string()));
        fm.insert(
            "source".to_string(),
            serde_yaml::Value::String("https://example.com".to_string()),
        );
        let result = render_frontmatter(&fm, "\n# Body\n");
        let lines: Vec<&str> = result.lines().collect();
        // title should come before type
        let title_pos = lines.iter().position(|l| l.contains("title")).expect("title");
        let type_pos = lines.iter().position(|l| l.contains("type")).expect("type");
        assert!(title_pos < type_pos);
    }

    #[test]
    fn test_render_frontmatter_tags() {
        let mut fm = HashMap::new();
        fm.insert(
            "tags".to_string(),
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::String("ai".to_string()),
                serde_yaml::Value::String("rust".to_string()),
            ]),
        );
        let result = render_frontmatter(&fm, "\n");
        assert!(result.contains("tags:\n  - ai\n  - rust"));
    }

    #[test]
    fn test_reclassify_type_youtube() {
        assert_eq!(reclassify_type("https://www.youtube.com/watch?v=abc123"), "youtube");
        assert_eq!(reclassify_type("https://youtu.be/abc123"), "youtube");
        assert_eq!(reclassify_type("https://www.youtube.com/shorts/abc123"), "youtube");
    }

    #[test]
    fn test_reclassify_type_github() {
        assert_eq!(reclassify_type("https://github.com/open-webui/open-terminal"), "github");
        assert_eq!(reclassify_type("https://github.com/Infatoshi/OpenSquirrel/"), "github");
    }

    #[test]
    fn test_reclassify_type_github_deep_path_is_article() {
        assert_eq!(
            reclassify_type("https://github.com/owner/repo/blob/main/README.md"),
            "article"
        );
        assert_eq!(reclassify_type("https://github.com/owner/repo/issues/42"), "article");
    }

    #[test]
    fn test_reclassify_type_social() {
        assert_eq!(
            reclassify_type("https://x.com/Zai_org/status/2033221428640674015"),
            "social"
        );
    }

    #[test]
    fn test_reclassify_type_reddit() {
        assert_eq!(
            reclassify_type("https://www.reddit.com/r/footballstrategy/comments/lhb3ku/help/"),
            "reddit"
        );
    }

    #[test]
    fn test_reclassify_type_article() {
        assert_eq!(reclassify_type("https://blog.example.com/post"), "article");
        assert_eq!(
            reclassify_type("https://www.xda-developers.com/some-article/"),
            "article"
        );
    }
}
