//! Regression guard (required by
//! `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md`,
//! Phase 3): the LIVE publish path hashes freshly-fetched member bodies
//! (`harvest::publish::publish_thread_inner` -> `watermark::thread_body_text`
//! -> `watermark::body_hash`), while a stage-2 REPLAY hashes the staged
//! `body.txt` (`pipeline::session::process_session_inner`, over the bytes
//! `replay::replay_session_stage2` read back out of staging).
//!
//! The two agree only because staging wrote exactly those bytes. Nothing
//! asserted it: a change to `thread_body_text`'s member separator or role
//! formatting would silently break `harvest-body-hash:` continuity - the
//! confirmation guard would start rejecting, and crash-recovery (resolution
//! step 3) would stop firing, with no test failing. That is prior attempt 1's
//! exact failure mode (an invariant living in one file, falsified in another).

#![allow(clippy::unwrap_used)]

mod common;

use borg::harvest::watermark;
use borg::stages::artifact::{ArtifactStore, FsArtifactStore};
use tempfile::TempDir;

#[tokio::test]
async fn the_live_and_staged_body_hashes_agree_byte_for_byte() {
    let _sandbox = common::XdgSandbox::new();
    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();

    let published = common::publish_one_thread(vault_dir.path(), staging_dir.path()).await;

    // The LIVE path: rebuild the canonical thread text from the same fetched
    // member bodies the publish runner hashed.
    let member_bodies: Vec<(String, Vec<_>)> = published
        .bodies
        .iter()
        .map(|(id, msgs)| (id.clone(), msgs.clone()))
        .collect();
    let live_text = watermark::thread_body_text(&member_bodies);
    let live_hash = watermark::body_hash(&live_text);

    // The REPLAY path: the bytes staging holds, which is what a stage-2 replay
    // re-derives (and re-hashes) from.
    let store = FsArtifactStore::from_config(&published.config.staging);
    let staged_bytes = store.read_body(&published.trace_id).unwrap();
    let staged_text = String::from_utf8(staged_bytes).unwrap();

    assert_eq!(
        staged_text, live_text,
        "staging must hold the canonical thread text byte-for-byte - a replay re-derives \
         the note (and its harvest-body-hash:) from these bytes"
    );
    assert_eq!(
        watermark::body_hash(&staged_text),
        live_hash,
        "the staged-body hash must equal the live fetched-body hash"
    );

    // ... and the watermark the live run actually recorded agrees with both,
    // so the value the re-appearance check compares against is the same value
    // a replay would compute.
    let recorded = &published.state.published[&published.primary_id].body_hash;
    assert_eq!(recorded, &live_hash, "the watermark records the live-path hash");

    // ... and so does the `harvest-body-hash:` the published note carries,
    // which is what the confirmation guard and the crash-recovery fallback
    // read (design doc: Data Model).
    let note_path = published.state.published[&published.primary_id].note_path.clone();
    let text = std::fs::read_to_string(&note_path).unwrap();
    let (yaml, _body) = vault::frontmatter::split_raw(&text).unwrap();
    let fm: serde_yaml::Mapping = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        fm.get("harvest-body-hash").and_then(|v| v.as_str()),
        Some(live_hash.as_str()),
        "the note's harvest-body-hash: is the same hash both paths compute"
    );
}
