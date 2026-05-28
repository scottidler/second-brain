//! Derive a `repo_slug` (e.g. `scottidler/second-brain`) from a Claude
//! Code session's `cwd` by walking up to the first ancestor that
//! contains a `.git/` directory.
//!
//! Falls back to `None` if the cwd does not sit inside a git
//! repository. Multi-repo sessions (the rare case where Scott's cwd
//! jumps repos mid-session) keep the slug of the cwd at session
//! start; the classifier flags such sessions for quarantine review.

use std::path::{Path, PathBuf};

const REPOS_PREFIX: &str = "repos/";

/// Resolve a (repo_path, repo_slug) pair for a session's cwd. The
/// slug is the `<org>/<repo>` portion of the path immediately after
/// the user's `~/repos/` root, mirroring Scott's repo layout. If the
/// cwd is not inside `~/repos/` or no `.git/` ancestor exists, returns
/// `(None, None)`.
pub fn resolve(cwd: &Path) -> (Option<PathBuf>, Option<String>) {
    log::debug!("repo::resolve: cwd={}", cwd.display());
    let repo_path = find_git_root(cwd);
    let slug = repo_path.as_deref().and_then(slug_from_repo_path);
    (repo_path, slug)
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur: &Path = start;
    loop {
        if cur.join(".git").is_dir() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

fn slug_from_repo_path(repo_path: &Path) -> Option<String> {
    let s = repo_path.to_string_lossy();
    let idx = s.find(REPOS_PREFIX)?;
    let after = &s[idx + REPOS_PREFIX.len()..];
    let mut iter = after.split('/');
    let org = iter.next()?;
    let repo = iter.next()?;
    if org.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{org}/{repo}"))
}

#[cfg(test)]
mod tests;
