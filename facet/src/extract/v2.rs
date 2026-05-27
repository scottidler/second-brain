//! Facet v2 extractor: multi-turn dialog-slice gems.
//!
//! Per the design doc, a gem is the four-part anatomy from the
//! Shopify-CEO talk (task, context, interaction, review) with verbatim
//! AI output preserved alongside Scott's. This module owns the chunker
//! and the per-cluster_assignment mining function.
//!
//! The mining function returns `Vec<Gem>`; persistence (upsert against
//! the `gems` and `interaction_turns` tables) goes through
//! `ledger::gems::upsert_gem`.

pub mod chunker;
pub mod gems;
