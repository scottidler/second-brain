//! `journal`: make sure today's daily note exists, so collectors that write
//! into it (the mini's `daily-spend-update`, future metric scripts) have a note
//! to write to even on a day nobody opened Obsidian.
//!
//! Where the note lives and what it starts from are Obsidian's own settings
//! (`.obsidian/daily-notes.json`: `folder`, `format`, `template`), read here
//! rather than copied into cortex config, so the note cortex creates is the
//! note Obsidian would have opened. The note is created with `create_new`:
//! one that already exists (Obsidian or another host got there first) is
//! never touched.
//!
//! Template rendering covers the core Templates plugin's date tokens
//! (`{{date}}`, `{{date:FORMAT}}`, `{{title}}`). Any other `{{...}}` token is
//! an error rather than a literal left in the note.

use chrono::NaiveDate;
use eyre::{Context, Result, eyre};
use serde::Deserialize;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Obsidian's settings file for the core Daily notes plugin, relative to the vault root.
pub const DAILY_NOTES_SETTINGS: &str = ".obsidian/daily-notes.json";

/// Obsidian's default daily note format, used when the settings leave it blank.
const DEFAULT_FORMAT: &str = "YYYY-MM-DD";

/// The subset of `.obsidian/daily-notes.json` that decides the note's path and body.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DailyNotesSettings {
    pub folder: String,
    pub format: String,
    pub template: String,
}

/// What [`ensure`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Ensured {
    Created(PathBuf),
    Exists(PathBuf),
}

/// Read the vault's daily-notes settings. A missing file is an error: without
/// it there is no agreed location, and guessing one forks the journal.
pub fn load_settings(vault_root: &Path) -> Result<DailyNotesSettings> {
    let path = vault_root.join(DAILY_NOTES_SETTINGS);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Create the daily note for `date` from the configured template unless it already exists.
pub fn ensure(vault_root: &Path, date: NaiveDate) -> Result<Ensured> {
    log::debug!("journal::ensure: vault_root={} date={date}", vault_root.display());
    let settings = load_settings(vault_root)?;
    let format = if settings.format.is_empty() {
        DEFAULT_FORMAT
    } else {
        settings.format.as_str()
    };
    let relative = format!("{}.md", format_date(format, date)?);
    let path = vault_root.join(&settings.folder).join(&relative);

    if path.exists() {
        log::debug!("journal::ensure: exists {}", path.display());
        return Ok(Ensured::Exists(path));
    }

    let template_path = template_path(vault_root, &settings.template)
        .ok_or_else(|| eyre!("{DAILY_NOTES_SETTINGS} names no template"))?;
    let template = std::fs::read_to_string(&template_path)
        .with_context(|| format!("reading daily note template {}", template_path.display()))?;
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| eyre!("daily note path has no file stem: {}", path.display()))?;
    let body = render(&template, date, title)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(body.as_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
            log::info!("journal: created {}", path.display());
            Ok(Ensured::Created(path))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(Ensured::Exists(path)),
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

/// Obsidian stores the template as a vault path without the `.md` extension.
fn template_path(vault_root: &Path, template: &str) -> Option<PathBuf> {
    if template.is_empty() {
        return None;
    }
    let path = vault_root.join(template);
    Some(if path.extension().is_some_and(|e| e == "md") {
        path
    } else {
        path.with_extension("md")
    })
}

/// Render a Moment.js-style date format. Only the tokens a daily note path
/// or date property needs are supported; any other letter is an error, never
/// copied through as a literal.
pub fn format_date(format: &str, date: NaiveDate) -> Result<String> {
    let mut out = String::new();
    let mut rest = format;
    while let Some(c) = rest.chars().next() {
        let (rendered, consumed) = if rest.starts_with("YYYY") {
            (date.format("%Y").to_string(), 4)
        } else if rest.starts_with("MM") {
            (date.format("%m").to_string(), 2)
        } else if rest.starts_with("DD") {
            (date.format("%d").to_string(), 2)
        } else if c.is_ascii_alphabetic() {
            return Err(eyre!("unsupported date token at {rest:?} in format {format:?}"));
        } else {
            (c.to_string(), c.len_utf8())
        };
        out.push_str(&rendered);
        rest = &rest[consumed..];
    }
    Ok(out)
}

/// Fill a core Templates plugin template's `{{date}}`, `{{date:FORMAT}}` and `{{title}}` tokens.
pub fn render(template: &str, date: NaiveDate, title: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| eyre!("unterminated template token at {:?}", &rest[start..]))?;
        let token = after[..end].trim();
        let value = match token.split_once(':') {
            Some(("date", fmt)) => format_date(fmt.trim(), date)?,
            None if token == "date" => format_date(DEFAULT_FORMAT, date)?,
            None if token == "title" => title.to_string(),
            _ => return Err(eyre!("unsupported template token {{{{{token}}}}}")),
        };
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests;
