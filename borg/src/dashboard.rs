use crate::config::Config;
use eyre::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const DASHBOARD_CONTENT: &str = r#"---
title: Borg Dashboard
date: {date}
type: system
domain: system
origin: authored
tags:
  - obsidian-borg
  - system
---

# Borg Dashboard

> Requires the [Dataview](https://github.com/blacksmithgu/obsidian-dataview) plugin.

> Queries pivot on `origin = "assisted"` (every borg-produced note) and on `ingested` (the date borg last touched the note - bumped on every reingest). `date` remains the original content date.

## 📥 Added Today

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE origin = "assisted" AND ingested = date(today)
SORT file.ctime DESC
```

## 📅 Yesterday

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE origin = "assisted" AND ingested = date(today) - dur(1 day)
SORT file.ctime DESC
```

## 📆 This Week

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE origin = "assisted" AND ingested >= date(today) - dur(7 day) AND ingested < date(today) - dur(1 day)
SORT file.ctime DESC
```

## 📅 This Month

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE origin = "assisted" AND ingested >= date(today) - dur(30 day) AND ingested < date(today) - dur(7 day)
SORT file.ctime DESC
```

## 📊 Stats

```dataview
TABLE WITHOUT ID
  length(rows) as "Count",
  rows.method as "Methods"
WHERE origin = "assisted"
GROUP BY type
```

## ⚠️ Recently failed (DLQ)

The full DLQ table lives at [[borg-dlq]]; this panel surfaces still-pending failures from the last week so they show up here without a manual click.

## 🕳️ Intake without resolution (orphans)

`borg audit` writes [[borg-orphans]] when an intake row has neither a ledger nor DLQ row within the deadline window. If that page is empty, the intake-log invariant is currently clean.
"#;

/// Resolve the dashboard path from config.
pub fn dashboard_path(config: &Config) -> Result<PathBuf> {
    let root = config.vault_root()?;
    Ok(root.join("system").join("views").join("borg-dashboard.md"))
}

/// Create the Borg Dashboard file if it doesn't exist.
pub fn ensure_dashboard_exists(dashboard_path: &Path) -> Result<()> {
    if dashboard_path.exists() {
        return Ok(());
    }
    if let Some(parent) = dashboard_path.parent() {
        fs::create_dir_all(parent).context("Failed to create dashboard directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let content = DASHBOARD_CONTENT.replace("{date}", &date);
    fs::write(dashboard_path, content).context("Failed to create Borg Dashboard")?;
    log::info!("Created Borg Dashboard at {}", dashboard_path.display());
    Ok(())
}

/// Rewrite the dashboard file with the current canonical template. Used by
/// `borg dashboard refresh` to upgrade dashboards that were generated
/// before a schema change (e.g. the source != null -> origin = "assisted"
/// + date -> ingested swap from the 2026-05-11 intake-log + DLQ design).
pub fn refresh(dashboard_path: &Path) -> Result<()> {
    if let Some(parent) = dashboard_path.parent() {
        fs::create_dir_all(parent).context("Failed to create dashboard directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let content = DASHBOARD_CONTENT.replace("{date}", &date);
    fs::write(dashboard_path, content).context("Failed to refresh Borg Dashboard")?;
    log::info!("Refreshed Borg Dashboard at {}", dashboard_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dashboard_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("obsidian-borg-test-dashboard");
        fs::create_dir_all(&dir).ok();
        dir.join(name)
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_ensure_dashboard_creates_file() {
        let path = temp_dashboard_path("test-create-dashboard.md");
        cleanup(&path);
        ensure_dashboard_exists(&path).expect("should create");
        assert!(path.exists());
        let content = fs::read_to_string(&path).expect("read");
        assert!(content.contains("# Borg Dashboard"));
        assert!(content.contains("dataview"));
        assert!(content.contains("Added Today"));
        cleanup(&path);
    }

    #[test]
    fn test_ensure_dashboard_idempotent() {
        let path = temp_dashboard_path("test-idempotent-dashboard.md");
        cleanup(&path);
        ensure_dashboard_exists(&path).expect("first");
        let content1 = fs::read_to_string(&path).expect("read");
        ensure_dashboard_exists(&path).expect("second");
        let content2 = fs::read_to_string(&path).expect("read");
        assert_eq!(content1, content2);
        cleanup(&path);
    }

    #[test]
    fn test_dashboard_has_all_sections() {
        let path = temp_dashboard_path("test-sections-dashboard.md");
        cleanup(&path);
        ensure_dashboard_exists(&path).expect("create");
        let content = fs::read_to_string(&path).expect("read");
        assert!(content.contains("Added Today"));
        assert!(content.contains("Yesterday"));
        assert!(content.contains("This Week"));
        assert!(content.contains("This Month"));
        assert!(content.contains("Stats"));
        cleanup(&path);
    }

    #[test]
    fn test_dashboard_uses_origin_assisted_not_source_null() {
        let path = temp_dashboard_path("test-origin-query.md");
        cleanup(&path);
        ensure_dashboard_exists(&path).expect("create");
        let content = fs::read_to_string(&path).expect("read");
        assert!(
            content.contains("origin = \"assisted\""),
            "dashboard should query origin = \"assisted\""
        );
        assert!(
            !content.contains("source != null"),
            "stale source != null filter should be gone"
        );
        cleanup(&path);
    }

    #[test]
    fn test_dashboard_pivots_on_ingested_not_date() {
        let path = temp_dashboard_path("test-ingested-pivot.md");
        cleanup(&path);
        ensure_dashboard_exists(&path).expect("create");
        let content = fs::read_to_string(&path).expect("read");
        assert!(
            content.contains("ingested = date(today)"),
            "Added Today panel should filter by ingested"
        );
        assert!(
            content.contains("ingested = date(today) - dur(1 day)"),
            "Yesterday panel should filter by ingested"
        );
    }

    #[test]
    fn test_refresh_dashboard_overwrites_existing() {
        let path = temp_dashboard_path("test-refresh-dashboard.md");
        cleanup(&path);
        fs::write(&path, "STALE CONTENT").expect("seed");
        refresh(&path).expect("refresh");
        let content = fs::read_to_string(&path).expect("read");
        assert!(!content.contains("STALE CONTENT"));
        assert!(content.contains("# Borg Dashboard"));
        cleanup(&path);
    }
}
