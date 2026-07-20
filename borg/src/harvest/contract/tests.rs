#![allow(clippy::unwrap_used)]
use super::*;

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/harvest/golden-2026-07-02.json"
);
const PHASE0_BULK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/distill-fixtures/session/bulk-envelope.json"
);
const PHASE0_BODY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/distill-fixtures/session/with-body-envelope.json"
);
const NULL_STRING_FIELDS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/distill-fixtures/session/null-string-fields.json"
);
const EMPTY_BODY: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/distill-fixtures/session/empty-body.json"
);
const MALFORMED_RECORD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/distill-fixtures/session/malformed-record.json"
);

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

#[test]
fn parses_golden_fixture() {
    let export = parse_export(&read(GOLDEN)).unwrap();
    assert_eq!(export.schema_version, 1);
    assert_eq!(export.cursor, 1500);
    assert_eq!(export.sessions.len(), 4);
}

#[test]
fn parses_phase0_bulk_envelope() {
    let export = parse_export(&read(PHASE0_BULK)).unwrap();
    assert_eq!(export.sessions.len(), 8);
    // present-null repo deserializes to None, not omitted-as-error.
    let personal = export
        .sessions
        .iter()
        .find(|s| s.session_id == "00849874-7c75-46a5-9975-8355c1835b12")
        .unwrap();
    assert_eq!(personal.repo, None);
    assert_eq!(personal.enrich_status, Some(EnrichStatus::SkippedPersonal));
    // a real repo session
    let marquee = export
        .sessions
        .iter()
        .find(|s| s.session_id == "0ed5c94c-ae33-4148-af76-d8c90a9a9571")
        .unwrap();
    assert_eq!(marquee.repo.as_deref(), Some("tatari-tv/marquee"));
    assert_eq!(marquee.enrich_status, Some(EnrichStatus::Ok));
    assert_eq!(marquee.git_branch.as_deref(), Some("main"));
}

#[test]
fn enrich_status_null_and_failed_round_trip() {
    let export = parse_export(&read(PHASE0_BULK)).unwrap();
    // enrich-status: null -> None
    let null_status = export
        .sessions
        .iter()
        .find(|s| s.session_id == "1ae69afa-24e9-42a2-956e-a5dcb8618f77")
        .unwrap();
    assert_eq!(null_status.enrich_status, None);
    // enrich-status: failed
    let failed = export
        .sessions
        .iter()
        .find(|s| s.session_id == "84594e71-752e-4fe9-a417-0b88553629b2")
        .unwrap();
    assert_eq!(failed.enrich_status, Some(EnrichStatus::Failed));
}

#[test]
fn repos_touched_is_three_state_none_when_omitted() {
    // Phase 0 fixtures predate files-touched, so repos-touched is OMITTED on
    // every session -> None (unknowable), NOT Some(vec![]) (touched nothing).
    let export = parse_export(&read(PHASE0_BULK)).unwrap();
    for s in &export.sessions {
        assert_eq!(
            s.repos_touched, None,
            "session {} should have None repos-touched",
            s.session_id
        );
    }
}

#[test]
fn repos_touched_three_states_are_distinct() {
    // Explicit proof that None (omitted), Some(vec![]) (present-empty), and
    // Some(xs) (populated) all deserialize distinctly - a default-empty Vec
    // would collapse the first two.
    let omitted: SessionRecord = serde_json::from_value(serde_json::json!({
        "session-id": "x", "host": "desk", "scope": "work", "cwd": "/c",
        "created": "2026-07-01T00:00:00+00:00", "modified": "2026-07-01T01:00:00+00:00",
        "dormant": true, "title": "t", "n-msgs": 10
    }))
    .unwrap();
    assert_eq!(omitted.repos_touched, None);

    let empty: SessionRecord = serde_json::from_value(serde_json::json!({
        "session-id": "x", "host": "desk", "scope": "work", "cwd": "/c",
        "created": "2026-07-01T00:00:00+00:00", "modified": "2026-07-01T01:00:00+00:00",
        "dormant": true, "title": "t", "n-msgs": 10, "repos-touched": []
    }))
    .unwrap();
    assert_eq!(empty.repos_touched, Some(vec![]));

    let populated: SessionRecord = serde_json::from_value(serde_json::json!({
        "session-id": "x", "host": "desk", "scope": "work", "cwd": "/c",
        "created": "2026-07-01T00:00:00+00:00", "modified": "2026-07-01T01:00:00+00:00",
        "dormant": true, "title": "t", "n-msgs": 10, "repos-touched": ["a/b", "c/d"]
    }))
    .unwrap();
    assert_eq!(
        populated.repos_touched,
        Some(vec!["a/b".to_string(), "c/d".to_string()])
    );
}

#[test]
fn git_branch_present_null_deserializes_to_none() {
    let rec: SessionRecord = serde_json::from_value(serde_json::json!({
        "session-id": "x", "host": "desk", "scope": "work", "cwd": "/c",
        "repo": null, "git-branch": null,
        "created": "2026-07-01T00:00:00+00:00", "modified": "2026-07-01T01:00:00+00:00",
        "dormant": true, "title": "t", "n-msgs": 10
    }))
    .unwrap();
    assert_eq!(rec.repo, None);
    assert_eq!(rec.git_branch, None);
}

#[test]
fn with_body_payload_parses_body_array() {
    let rec = parse_export(&read(PHASE0_BODY)).unwrap().sessions.remove(0);
    let body = rec.body.expect("with-body payload has a body array");
    assert!(!body.is_empty());
    assert!(body.iter().any(|m| m.role == "user"));
    assert!(!rec.body_truncated);
    assert_eq!(rec.body_error, None);
}

// ---- harvest-completion Phase 0 spike (docs/design/2026-07-20-harvest-completion.md):
// real null-bearing fixtures, RED against the current whole-batch, non-Option
// `cwd`/`created`/`title`/`first_prompt` contract. This test documents and
// LOCKS the exact production bug (`sb borg harvest --dry-run --since 60d` ->
// "invalid type: null, expected a string at line 796 column 19") until Phase 1
// relaxes the types to present-null `Option<String>` and switches to
// per-record deserialization. Do NOT make this pass by touching contract.rs
// in this phase - it is SUPPOSED to fail here (the `match` panic below is
// the point of the spike, not a bug in the test).
//
// NOTE: assertions below deliberately stick to fields that are ALREADY
// `Option<...>` today (`repo`/`git_branch`/`model`/`summary`/`body`/
// `body_error`) so this test file compiles cleanly against today's
// `contract.rs` (where `cwd`/`created`/`title`/`first_prompt` are plain,
// non-Option `String`). Once Phase 1 relaxes those four fields, extend this
// test with direct `None` assertions on them too.
#[test]
fn parse_tolerates_null_string_fields() {
    // `9b17cdba-...` in this real export carries `cwd`, `created`, `title`,
    // and `first-prompt` all as JSON `null` (a genuinely empty/never-touched
    // session, `n-msgs: 0`), alongside real rows where only some of those
    // fields are null. Today `cwd`/`created`/`title` are plain `String` and
    // `first_prompt` is `String` with `#[serde(default)]` (which only
    // covers an OMITTED key, not present-null) - so this call fails with
    // clyde's exact production error, not a clean `Ok` result.
    let result = parse_export(&read(NULL_STRING_FIELDS));
    let export = match result {
        Ok(export) => export,
        Err(err) => panic!(
            "parse_export must tolerate null cwd/created/title/first-prompt \
             (locks the docs/design/2026-07-20-harvest-completion.md Problem #1 bug); \
             currently fails with: {err:#}"
        ),
    };
    assert_eq!(
        export.sessions.len(),
        5,
        "all five real sessions should survive parsing"
    );

    let empty_bomb = export
        .sessions
        .iter()
        .find(|s| s.session_id == "9b17cdba-7995-4be9-a1a4-65af5e7a3250")
        .expect("empty-bomb session present");
    // Already-Option null classes ride along in the same real record;
    // these are safe to assert both today and after Phase 1.
    assert_eq!(empty_bomb.repo, None);
    assert_eq!(empty_bomb.git_branch, None);
    assert_eq!(empty_bomb.model, None);
    assert_eq!(empty_bomb.summary, None);

    // Real, non-null classes must survive the relaxation unchanged.
    let work_session = export
        .sessions
        .iter()
        .find(|s| s.session_id == "687368e0-a239-4b66-8a5e-d3c6d3b6af0f")
        .expect("real work session present");
    assert_eq!(work_session.repo.as_deref(), Some("scottidler/claude"));
    assert_eq!(work_session.model, None);

    // A real `--id --with-body` export where clyde itself reports an empty
    // transcript (`body: null`, `body-error: "parsed empty"`) - its
    // title/cwd/created are null too, so it fails the identical way today.
    let body_export =
        parse_export(&read(EMPTY_BODY)).unwrap_or_else(|err| panic!("empty-body export must parse: {err:#}"));
    let empty_session = &body_export.sessions[0];
    assert_eq!(empty_session.body, None);
    assert_eq!(empty_session.body_error.as_deref(), Some("parsed empty"));

    // A synthetic-malformed record (real shape, `n-msgs` wrong-typed) -
    // today's whole-batch parser rejects it outright; Phase 1's per-record
    // resilience turns this into a per-record skip instead of a batch abort.
    assert!(
        parse_export(&read(MALFORMED_RECORD)).is_err(),
        "current whole-batch parser must reject a malformed element (locks the pre-Phase-1 shape)"
    );
}

#[test]
fn schema_version_mismatch_is_a_loud_error() {
    let payload = br#"{"schema-version": 2, "cursor": 1, "sessions": []}"#;
    let err = parse_export(payload).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("unsupported schema-version 2"), "got: {msg}");
}

#[test]
fn garbage_is_a_loud_error_never_empty() {
    let err = parse_export(b"not json at all").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not valid JSON") || msg.contains("schema-version"),
        "got: {msg}"
    );
}

#[test]
fn clyde_uri_shape() {
    let rec: SessionRecord = serde_json::from_value(serde_json::json!({
        "session-id": "abc-123", "host": "desk", "scope": "work", "cwd": "/c",
        "created": "2026-07-01T00:00:00+00:00", "modified": "2026-07-01T01:00:00+00:00",
        "dormant": true, "title": "t", "n-msgs": 10
    }))
    .unwrap();
    assert_eq!(rec.clyde_uri(), "clyde://abc-123");
}
