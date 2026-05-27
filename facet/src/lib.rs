#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]
// Lib invariant: facet pub fns return typed data; sb owns stdout/stderr.
// Production code emits nothing via println!/eprintln!. Test modules that
// print captured stdout opt in via #[cfg_attr(test, allow(...))] on the
// test declaration.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

//! facet - judgment-moment harvester for Claude Code JSONL transcripts.
//!
//! Mirrors the borg/cortex/oracle subsystem shape. Reads JSONL transcripts
//! under `~/.claude/projects/`, clusters turns into cross-session
//! work-items, mines moments of senior judgment, and renders one evolving
//! markdown note per work-item into the obsidian vault.
//!
//! End-to-end pipeline per tick:
//! 1. [`scan`]    - enumerate JSONL files; parse new-turn slices.
//! 2. [`workitem::cluster`] - assign new turns to work-items via the
//!    cluster LLM; persist `cluster_assignments` rows.
//! 3. [`extract`] - per (session, cluster_assignment), mine judgment
//!    moments via the extract LLM.
//! 4. [`render`]  - fencepost-merge work-item notes into the vault.
//!
//! Ledger schema-of-record: [`ledger`].

pub mod config;
pub mod daemon;
pub mod dedupe;
pub mod dream;
pub mod extract;
pub mod fabric;
pub mod gems;
pub mod jsonl;
pub mod ledger;
pub mod narrative;
pub mod notify;
pub mod render;
pub mod repo;
pub mod scan;
pub mod workitem;
pub mod yaml_out;

pub use config::Config;
pub use ledger::Ledger;
