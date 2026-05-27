//! Facet v1 extractor: paraphrased one-line judgment moments.
//!
//! Retained intact during the v2 cutover (per the design doc Migration
//! Plan). Selected by the `--v1` flag on `sb facet harvest`. Phase 7
//! drops this module and the underlying `judgment_moments` tables.
//!
//! See `facet/src/extract.rs` for the dispatcher that chooses between
//! v1 (this module) and v2 (`facet/src/extract/v2.rs`).

pub mod mine;
