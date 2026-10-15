//! `sb borg eval --patterns-dir <DIR>`: resolve every pattern the eval
//! distills or judges through from DIR instead of the deployed
//! `~/.config/sb/patterns/`, so a prompt change in the repo is measured
//! without deploying it (`--patterns-dir borg/patterns`).
//!
//! Local to the eval: live ingest resolves through `vault::fabric::resolve_pattern`
//! unchanged. The override hands fabric an absolute file path, which
//! `resolve_pattern` passes through as is.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use distillers::{FabricCaller, FabricRequest};
use eyre::{Context, Result, bail};

/// Canonicalize a user-supplied patterns dir at the boundary: tilde-expanded,
/// absolute, and required to exist as a directory.
pub fn resolve_patterns_dir(dir: &Path) -> Result<PathBuf> {
    let expanded = vault::paths::expand_tilde(dir);
    let absolute =
        std::fs::canonicalize(&expanded).with_context(|| format!("--patterns-dir {} not found", expanded.display()))?;
    if !absolute.is_dir() {
        bail!("--patterns-dir {} is not a directory", absolute.display());
    }
    Ok(absolute)
}

/// The pattern argument for fabric: `name` unchanged without an override
/// (deployed resolution), else the absolute `DIR/<name>.md`, which must exist.
pub fn pattern_in(dir: Option<&Path>, name: &str) -> Result<String> {
    let Some(dir) = dir else {
        return Ok(name.to_string());
    };
    let file = if name.ends_with(".md") {
        dir.join(name)
    } else {
        dir.join(format!("{name}.md"))
    };
    if !file.is_file() {
        bail!("pattern '{name}' not found in --patterns-dir: {}", file.display());
    }
    Ok(file.to_string_lossy().to_string())
}

/// A `FabricCaller` that rewrites each request's pattern through
/// [`pattern_in`] before delegating, so every distiller the dispatcher routes
/// to reads its prompt from the override dir.
#[derive(Debug, Clone)]
pub struct PatternDirFabric<F> {
    pub inner: F,
    pub dir: Option<PathBuf>,
}

#[async_trait]
impl<F: FabricCaller> FabricCaller for PatternDirFabric<F> {
    async fn call(&self, mut request: FabricRequest) -> Result<String> {
        request.pattern = pattern_in(self.dir.as_deref(), &request.pattern)?;
        log::debug!("PatternDirFabric::call: pattern={}", request.pattern);
        self.inner.call(request).await
    }
}

#[cfg(test)]
mod tests;
