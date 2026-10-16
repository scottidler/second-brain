//! Test-only stand-in for `spawn::yt_dlp`: a task-local scope that counts
//! and answers every call; no real process.

use super::*;
use std::sync::{Arc, Mutex};

/// Answers one `yt-dlp` call from its argv.
pub(crate) type Stub = Arc<dyn Fn(&[String]) -> std::io::Result<Output> + Send + Sync>;

/// Every `yt-dlp` argv seen inside one [`scope`], in call order.
#[derive(Clone, Default)]
pub(crate) struct Spawns(Arc<Mutex<Vec<Vec<String>>>>);

impl Spawns {
    pub(crate) fn count(&self) -> usize {
        self.0.lock().expect("spawns lock").len()
    }

    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.0.lock().expect("spawns lock").clone()
    }

    /// How many calls carried `flag` as one argument.
    pub(crate) fn count_with(&self, flag: &str) -> usize {
        self.calls()
            .iter()
            .filter(|argv| argv.iter().any(|a| a == flag))
            .count()
    }
}

#[derive(Clone)]
struct Scope {
    spawns: Spawns,
    stub: Stub,
}

tokio::task_local! {
    static SCOPE: Scope;
}

/// Run `fut` with every `yt-dlp` spawn inside it counted and answered by
/// `stub`. Returns the future's output and the recorded calls.
pub(crate) async fn scope<F: Future>(stub: Stub, fut: F) -> (F::Output, Spawns) {
    let spawns = Spawns::default();
    let out = SCOPE
        .scope(
            Scope {
                spawns: spawns.clone(),
                stub,
            },
            fut,
        )
        .await;
    (out, spawns)
}

/// A finished process with `code` and the given output.
pub(crate) fn exit(code: i32, stdout: &str, stderr: &str) -> Output {
    use std::os::unix::process::ExitStatusExt;
    Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

pub(super) fn answer(args: &[&str]) -> std::io::Result<Run> {
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let scope = SCOPE.try_with(Scope::clone).map_err(|_| {
        std::io::Error::other("yt-dlp spawned outside a spawn::scope in a test; no real process is started")
    })?;
    scope.spawns.0.lock().expect("spawns lock").push(argv.clone());
    let output = (scope.stub)(&argv);
    Ok(Box::pin(async move { output }))
}
