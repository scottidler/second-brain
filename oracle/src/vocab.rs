//! The canonical tag vocabulary oracle reports, resolved the way borg does.
//!
//! One key, one file: borg.yml's `tags.canonical-path`, read through the same
//! `BorgView` the queue client uses. A key left unset falls back to
//! `vault::paths::canonical_tags()`, borg's own default, so oracle and borg
//! cannot name different files. Every failure is an error naming the file;
//! nothing here turns a failed read into an empty vocabulary.

use eyre::{Context, bail};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::queue::BorgView;

/// Resolve the vocabulary path from borg.yml at `borg_yml`.
///
/// - borg.yml absent (`fs::metadata` says `NotFound`): the default path.
/// - borg.yml present but unreadable or unparseable: an error naming borg.yml.
///   `Path::exists()` is not used because it folds "unreadable" into "absent".
/// - `tags.canonical-path` set: that path, tilde-expanded.
pub fn vocabulary_path(borg_yml: &Path) -> eyre::Result<PathBuf> {
    debug!("vocab::vocabulary_path: borg_yml={}", borg_yml.display());
    match std::fs::metadata(borg_yml) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {
            debug!("vocab::vocabulary_path: borg.yml absent, using the default path");
            return Ok(vault::paths::canonical_tags());
        }
        Err(e) => bail!("cannot read {}: {e}", borg_yml.display()),
    }
    let view = crate::queue::load_view(Some(&borg_yml.to_path_buf()))?;
    Ok(configured_path(&view))
}

fn configured_path(view: &BorgView) -> PathBuf {
    match &view.tags.canonical_path {
        Some(raw) => vault::paths::expand_tilde(raw),
        None => vault::paths::canonical_tags(),
    }
}

/// Load and sort the vocabulary at `path`. The error is `"<path>: <cause>"`.
pub fn load_canonical_tags(path: &Path) -> eyre::Result<Vec<String>> {
    debug!("vocab::load_canonical_tags: path={}", path.display());
    let file = vault::canonical::CanonicalTagsFile::load(path).wrap_err_with(|| path.display().to_string())?;
    let mut tags: Vec<String> = file.all_tags().into_iter().collect();
    tags.sort();
    debug!("vocab::load_canonical_tags: {} tags", tags.len());
    Ok(tags)
}

/// Resolve and load in one step. `Err` is the display string the tools put in
/// `vocabulary-error`.
pub fn resolve(borg_yml: &Path) -> Result<Vec<String>, String> {
    let path = vocabulary_path(borg_yml).map_err(|e| format!("{e:#}"))?;
    load_canonical_tags(&path).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests;
