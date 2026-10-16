//! Test-only: point the gemini arm's clients at local stubs for one task,
//! since base URLs are not config. Task-local, so parallel tests never share.

use std::future::Future;

#[derive(Clone, Debug)]
pub(crate) struct ApiBases {
    pub(crate) data_api: String,
    pub(crate) gemini: String,
}

tokio::task_local! {
    static BASES: ApiBases;
}

pub(crate) async fn scope<F: Future>(bases: ApiBases, fut: F) -> F::Output {
    BASES.scope(bases, fut).await
}

pub(super) fn current() -> Result<ApiBases, super::FetchError> {
    BASES
        .try_with(ApiBases::clone)
        .map_err(|_| super::FetchError::Network("gemini engine used outside bases::scope in a test".to_string()))
}
