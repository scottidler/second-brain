//! The one place borg's ingest path starts a `yt-dlp` process.
//!
//! Every `yt-dlp` call in `youtube.rs` and `pipeline/handlers.rs` goes through
//! [`yt_dlp`], so a test can count them and answer them without a real
//! process. The caller keeps its own timeout around the returned future, and
//! dropping that future kills the child (`kill_on_drop`), exactly as the
//! inline `TokioCommand::new("yt-dlp")` calls did.
//!
//! Under `cfg(test)` there is no real process at all: a spawn inside
//! [`scope`] is counted and answered by the scope's stub, and a spawn outside
//! any scope is an error. The scope is a tokio task-local, so two tests
//! running in parallel each see only their own count.

use std::future::Future;
use std::pin::Pin;
use std::process::Output;

/// A started `yt-dlp` run: resolves to its exit status and captured output.
pub(crate) type Run = Pin<Box<dyn Future<Output = std::io::Result<Output>> + Send>>;

/// Start `yt-dlp args...` with stdout and stderr piped and `kill_on_drop`.
/// The `Err` is the spawn failure (binary missing); the caller adds context.
pub(crate) fn yt_dlp(args: &[&str]) -> std::io::Result<Run> {
    log::debug!("spawn::yt_dlp: args={args:?}");
    start(args)
}

#[cfg(not(test))]
fn start(args: &[&str]) -> std::io::Result<Run> {
    let child = tokio::process::Command::new("yt-dlp")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    Ok(Box::pin(child.wait_with_output()))
}

#[cfg(test)]
fn start(args: &[&str]) -> std::io::Result<Run> {
    fake::answer(args)
}

#[cfg(test)]
pub(crate) use fake::{Spawns, Stub, exit, scope};

#[cfg(test)]
mod fake;

#[cfg(test)]
mod tests;
