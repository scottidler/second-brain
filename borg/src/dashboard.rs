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

## 📥 Added Today

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE (source != null OR asset != null OR method != null) AND date = date(today)
SORT file.ctime DESC
```

## 📅 Yesterday

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE (source != null OR asset != null OR method != null) AND date = date(today) - dur(1 day)
SORT file.ctime DESC
```

## 📆 This Week

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE (source != null OR asset != null OR method != null) AND date >= date(today) - dur(7 day) AND date < date(today) - dur(1 day)
SORT file.ctime DESC
```

## 📅 This Month

```dataview
TABLE WITHOUT ID
  link(file.link, title) as "Title",
  type as "Type",
  method as "Via",
  domain as "Domain"
WHERE (source != null OR asset != null OR method != null) AND date >= date(today) - dur(30 day) AND date < date(today) - dur(7 day)
SORT file.ctime DESC
```

## 📊 Stats

```dataview
TABLE WITHOUT ID
  length(rows) as "Count",
  rows.method as "Methods"
WHERE source != null OR asset != null OR method != null
GROUP BY type
```
"#;

/// Resolve the dashboard path from config.
pub fn dashboard_path(config: &Config) -> PathBuf {
    let root = expand_tilde(&config.vault.root_path);
    root.join("system").join("borg-dashboard.md")
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
}
