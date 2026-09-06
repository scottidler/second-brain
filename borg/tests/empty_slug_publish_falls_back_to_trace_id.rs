//! Regression guard for Phase 4 (F1) of
//! `docs/design/2026-09-05-discovery-remediation.md`: a `kind: text` ingest
//! whose title sanitizes to empty (a title made entirely of box-drawing
//! characters) must land a note, not silently write `inbox/.md`. Drives the
//! REAL `borg::pipeline::process_content` entry point end to end, exactly
//! like a Telegram/Signal text capture would.

#![allow(clippy::unwrap_used)]

mod common;

use borg::pipeline;
use borg::types::{ContentKind, IngestMethod, IngestStatus};
use tempfile::TempDir;

#[tokio::test]
async fn box_drawing_title_lands_a_note_stemmed_untitled() {
    let _sandbox = common::XdgSandbox::new();
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();

    // `process_content` panics if the permit pools were never sized (normally
    // done by `serve_init`/CLI startup) - same setup `common::publish_one_thread`
    // uses.
    pipeline::permits::GENERAL_PERMITS.init(4);
    pipeline::permits::HEAVY_PERMITS.init(2);

    let config = common::test_config(vault_dir.path(), staging_dir.path());

    // Ten U+2500 (BOX DRAWINGS LIGHT HORIZONTAL): short enough to be taken
    // verbatim as the title (`generate_text_title`'s first-line branch), then
    // `vault::hygiene::sanitize_filename` strips every char, leaving an empty
    // slug - the exact case Phase 4 guards.
    let title_text = "\u{2500}".repeat(10);

    let result = pipeline::process_content(
        ContentKind::Text(title_text),
        Vec::new(),
        IngestMethod::Manual,
        false,
        &config,
        None,
        None,
    )
    .await;

    assert!(
        matches!(result.status, IngestStatus::Completed),
        "expected the text ingest to complete: {:?}",
        result
    );
    let note_path = result.note_path.expect("completed ingest carries a note_path");
    let stem = std::path::Path::new(&note_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("note_path has a filename stem");
    assert!(
        stem.starts_with("untitled-"),
        "box-drawing-only title must fall back to untitled-<trace_id>, got stem={stem:?}"
    );
}
