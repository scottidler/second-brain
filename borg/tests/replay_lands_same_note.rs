//! Regression guard (required by
//! `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md`,
//! Phase 3): publish a trace, re-publish the SAME trace with a deliberately
//! different injected slug, and assert exactly one note exists for that trace.
//!
//! Why this file exists at all: the FIRST attempt to fix this bug encoded
//! "re-deriving a trace yields the same path" as a comment in `replay.rs`, and
//! a naming change four days later in a DIFFERENT file (the filename stem
//! moved from clyde's session title to model output) falsified it with no test
//! noticing. Trace `hv-e5d240` then accumulated 15 notes in the live vault.
//! This guard fails if prior-note resolution is stubbed out, removed, or
//! bypassed - verified by stubbing it.

#![allow(clippy::unwrap_used)]

mod common;

use borg::harvest::SESSION_REPLAY_META_FILE;
use borg::stages::artifact::{ArtifactStore, FsArtifactStore};
use tempfile::TempDir;

/// Every `.md` file under `root`, recursively.
fn all_notes(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn replaying_a_trace_lands_on_the_note_it_already_produced() {
    let _sandbox = common::XdgSandbox::new();
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();

    let published = common::publish_one_thread(vault_dir.path(), staging_dir.path()).await;
    let landed = all_notes(vault_dir.path());
    assert_eq!(landed.len(), 1, "the harvest run lands exactly one note: {landed:?}");
    let original = landed[0].clone();

    // Inject a DIFFERENT slug for the replay: the staged member title is what
    // the (fabric-less, degraded) distill falls back to for the filename stem,
    // so rewriting it here reproduces exactly what the model did 15 times to
    // `hv-e5d240` - emit a different slug for the same input.
    let store = FsArtifactStore::from_config(&published.config.staging);
    let raw = store
        .read_attachment(&published.trace_id, SESSION_REPLAY_META_FILE)
        .unwrap()
        .expect("publish stages members.yml");
    let yaml = String::from_utf8(raw).unwrap();
    let mutated = yaml.replace(
        &format!("session {} work", published.primary_id),
        "a wholly different subject line",
    );
    assert_ne!(yaml, mutated, "the injected title must actually differ");
    store
        .write_attachment(&published.trace_id, SESSION_REPLAY_META_FILE, mutated.as_bytes())
        .unwrap();

    let report = borg::replay::run(
        published.config,
        borg::replay::ReplayOptions {
            trace_id: Some(published.trace_id.clone()),
            from_stage: 2,
            ..Default::default()
        },
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(report.succeeded, 1, "the stage-2 replay must succeed");
    assert_eq!(report.failed, 0);

    let after = all_notes(vault_dir.path());
    assert_eq!(
        after.len(),
        1,
        "one trace owns exactly one note, no matter how many times it is replayed \
         or how the model renames it: {after:?}"
    );
    assert_eq!(
        after[0], original,
        "the replay must rewrite the ORIGINAL file, so every inbound wikilink keeps resolving"
    );

    let text = std::fs::read_to_string(&original).unwrap();
    let (yaml, _body) = vault::frontmatter::split_raw(&text).unwrap();
    let fm: serde_yaml::Mapping = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        fm.get("trace").and_then(|v| v.as_str()),
        Some(published.trace_id.as_str())
    );
    assert_eq!(
        fm.get("slug").and_then(|v| v.as_str()),
        original.file_stem().and_then(|s| s.to_str()),
        "slug: names the file it lives in, not the slug the replay's distill produced"
    );
}
