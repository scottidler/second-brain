//! Test-only: point the Jina Reader fetch at a local stub for one task, since
//! the reader base URL is not config. Task-local, so parallel tests never share.

use std::future::Future;

tokio::task_local! {
    static BASE: String;
}

pub(crate) async fn scope<F: Future>(base: String, fut: F) -> F::Output {
    BASE.scope(base, fut).await
}

pub(super) fn current() -> eyre::Result<String> {
    BASE.try_with(String::clone)
        .map_err(|_| eyre::eyre!("Jina Reader used outside jina::bases::scope in a test"))
}
