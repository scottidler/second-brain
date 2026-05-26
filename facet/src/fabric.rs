//! Fabric integration.
//!
//! facet calls Fabric the same way borg/distillers do: via the
//! [`distillers::fabric::FabricCaller`] trait. Production uses
//! [`distillers::fabric::FabricShell`]; tests use
//! [`distillers::fabric::FakeFabric`].
//!
//! This module is a re-export plus a thin helper that constructs a
//! [`FabricRequest`] with facet-default char and timeout knobs.

pub use distillers::fabric::{FabricCaller, FabricRequest, FabricShell, FakeFabric};

/// Build a `FabricRequest` with facet's defaults baked in.
pub fn request(
    pattern: impl Into<String>,
    input: impl Into<String>,
    model: impl Into<String>,
    timeout_secs: u64,
) -> FabricRequest {
    FabricRequest {
        pattern: pattern.into(),
        input: input.into(),
        model: model.into(),
        max_chars: 200_000,
        timeout_secs,
    }
}
