//! Test-only: point the X ingest path at a local fxtwitter stub for one task,
//! since the API base URL is not config. Task-local, so parallel tests never share.

use std::future::Future;

tokio::task_local! {
    static BASE: String;
}

pub(crate) async fn scope<F: Future>(base: String, fut: F) -> F::Output {
    BASE.scope(base, fut).await
}

pub(super) fn current() -> eyre::Result<String> {
    BASE.try_with(String::clone)
        .map_err(|_| eyre::eyre!("fxtwitter API used outside x::bases::scope in a test"))
}
