//! The daemon's startup embedding-model load, with a stop check right after
//! it. The stop listener is installed before the load, so without the check a
//! stop that lands during the load would be held until the startup sweep
//! finished too.

use eyre::Result;
use vault::embedding::EmbeddingModel;

use crate::shutdown::StopHandle;

/// What the daemon has once the model load returns.
pub(super) enum StartupModel {
    /// A stop arrived while the model loaded: exit before the startup sweep.
    Stopped,
    /// The loaded model, or `None` when the load failed: the embed tick is
    /// disabled for this process and every other action still runs.
    Ready(Option<Box<dyn EmbeddingModel>>),
}

/// Run `load`, then read the stop flag as soon as it returns. The load itself
/// has no check point: a stop during a model mmap, or during a cold hf-hub
/// download, waits for it to return.
pub(super) fn load_unless_stopped<L>(stop: &StopHandle, load: L) -> StartupModel
where
    L: FnOnce() -> Result<Box<dyn EmbeddingModel>>,
{
    let model = match load() {
        Ok(model) => Some(model),
        Err(e) => {
            log::error!(
                "daemon: embedding model failed to load: {e:#}; embed tick DISABLED for this process \
                 - every non-embed action still runs"
            );
            None
        }
    };
    if stop.is_stopped() {
        log::info!("received shutdown signal during the embedding model load; shutting down before the startup sweep");
        return StartupModel::Stopped;
    }
    StartupModel::Ready(model)
}

#[cfg(test)]
mod tests;
