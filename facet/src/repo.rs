//! Resolve a cwd to an `owner/repo` slug via `git remote get-url origin`.
//!
//! Patterned on `claude-report`'s `repo::parse_slug` — same shape,
//! written from scratch (claude-report is not a runtime dependency;
//! see Alternative 2 in the design doc). The URL parsers cover the
//! four shapes `git remote get-url` emits: `git@host:org/repo[.git]`,
//! `https://host/org/repo[.git]`, `git://host/org/repo[.git]`, and
//! `ssh://[user@]host/org/repo[.git]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// One-shot helper. Returns `None` if the cwd has no git origin or the
/// origin URL does not resolve to a clean owner/repo slug.
pub fn resolve_slug(cwd: &Path) -> Option<String> {
    Resolver::new().resolve(cwd)
}

/// Cached resolver. Multiple sessions under the same cwd hit the same
/// cached lookup. The cache is per-instance, not global.
#[derive(Debug, Default)]
pub struct Resolver {
    cache: Mutex<HashMap<PathBuf, Option<String>>>,
    blocked: Vec<PathBuf>,
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            blocked: dirs::home_dir().map(|h| vec![h]).unwrap_or_default(),
        }
    }

    /// Resolve `cwd` to an `owner/repo` slug. Returns `None` for cwds
    /// without a git origin, cwds whose git toplevel is `$HOME`
    /// (avoids picking up a vagrant `~/.config` git checkout), or
    /// non-existent paths.
    pub fn resolve(&self, cwd: &Path) -> Option<String> {
        if let Ok(guard) = self.cache.lock()
            && let Some(v) = guard.get(cwd)
        {
            return v.clone();
        }
        let slug = self.detect(cwd);
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert(cwd.to_path_buf(), slug.clone());
        }
        slug
    }

    fn detect(&self, cwd: &Path) -> Option<String> {
        log::trace!("repo::detect: cwd={}", cwd.display());
        if !cwd.exists() {
            log::debug!("repo::detect: cwd missing on disk: {}", cwd.display());
            return None;
        }
        let toplevel = run_git(cwd, &["rev-parse", "--show-toplevel"])?;
        let toplevel = PathBuf::from(toplevel.trim());
        if !(toplevel == cwd || cwd.starts_with(&toplevel)) {
            log::debug!(
                "repo::detect: toplevel {} is not at or above cwd {}; rejecting",
                toplevel.display(),
                cwd.display()
            );
            return None;
        }
        if self.blocked.iter().any(|b| b == &toplevel) {
            log::debug!(
                "repo::detect: toplevel {} matches a blocked root (HOME); rejecting",
                toplevel.display()
            );
            return None;
        }
        let origin = run_git(cwd, &["remote", "get-url", "origin"])?;
        parse_slug(origin.trim())
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Convert an `origin` URL to an `owner/repo` slug. Returns `None` for
/// shapes that do not contain exactly one `owner/repo` segment pair.
pub fn parse_slug(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let path = if let Some(rest) = url.strip_prefix("git@") {
        let (_, path) = rest.split_once(':')?;
        path.to_string()
    } else if let Some(rest) = url.strip_prefix("https://") {
        let (_, path) = rest.split_once('/')?;
        path.to_string()
    } else if let Some(rest) = url.strip_prefix("http://") {
        let (_, path) = rest.split_once('/')?;
        path.to_string()
    } else if let Some(rest) = url.strip_prefix("git://") {
        let (_, path) = rest.split_once('/')?;
        path.to_string()
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let after_user = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        let (_, path) = after_user.split_once('/')?;
        path.to_string()
    } else {
        return None;
    };
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let (org, repo) = path.split_once('/')?;
    if org.is_empty() || repo.is_empty() {
        return None;
    }
    if repo.contains('/') {
        return None;
    }
    Some(format!("{org}/{repo}"))
}

#[cfg(test)]
mod tests;
