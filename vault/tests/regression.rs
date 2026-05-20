//! Integration-test entry point for regression suites under
//! `vault/tests/regression/`.
//!
//! Each module here owns one regression area. Cargo treats this file
//! itself as the integration test binary; the actual test bodies live
//! in `regression/<module>.rs` and are pulled in by `mod` declarations.
//! Latency / throughput work belongs in `vault/benches/`, not here.

#![cfg(feature = "vec")]

#[path = "regression/hybrid.rs"]
mod hybrid;

#[cfg(feature = "vec-candle")]
#[path = "regression/candle.rs"]
mod candle;
