//! Doctor check: borg and cortex must read the same canonical-tags file.

use std::path::Path;

use super::Finding;

/// Warn when borg's `tags.canonical-path` and cortex's `sweep.canonical-path`
/// differ; `None` when they match. Both are tilde-expanded before they get here.
pub(super) fn mismatch_finding(borg: &Path, cortex: &Path) -> Option<Finding> {
    if borg == cortex {
        return None;
    }
    Some(Finding::warn(
        format!(
            "borg and cortex read different canonical-tags files: borg tags.canonical-path = {}, cortex sweep.canonical-path = {}",
            borg.display(),
            cortex.display()
        ),
        "set both keys to the same path (or unset both) in borg.yml and cortex.yml",
    ))
}

/// The configs that do not load are reported by their own sections; this
/// check skips rather than reporting them twice.
pub(super) fn path_mismatch_findings(borg: &borg::config::Config) -> Vec<Finding> {
    match cortex::config::Config::load(None) {
        Ok(cortex_cfg) => mismatch_finding(&borg.tags.canonical_path, &cortex_cfg.sweep.canonical_path)
            .into_iter()
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
