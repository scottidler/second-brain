//! Test-only: point the vision client at a local stub for one task, since the
//! Anthropic base URL is not config. Task-local, so parallel tests never share.

use std::future::Future;

tokio::task_local! {
    static BASE: String;
}

pub(crate) async fn scope<F: Future>(base: String, fut: F) -> F::Output {
    BASE.scope(base, fut).await
}

pub(super) fn current() -> eyre::Result<String> {
    BASE.try_with(String::clone)
        .map_err(|_| eyre::eyre!("vision API used outside ocr::bases::scope in a test"))
}

/// Carry the calling task's stub base into `fut`, which is about to run on a
/// spawned task (task-locals do not cross `spawn`). Captured now, at the call.
pub(crate) fn carry<F: Future>(fut: F) -> impl Future<Output = F::Output> {
    let base = BASE.try_with(String::clone).ok();
    async move {
        match base {
            Some(base) => BASE.scope(base, fut).await,
            None => fut.await,
        }
    }
}
