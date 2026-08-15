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
    let export = parse_export(&read(GOLDEN)).unwrap().export;
    assert_eq!(export.schema_version, 1);
    assert_eq!(export.cursor, 1500);
    assert_eq!(export.sessions.len(), 4);
}

#[test]
fn parses_phase0_bulk_envelope() {
    let export = parse_export(&read(PHASE0_BULK)).unwrap().export;
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
    let export = parse_export(&read(PHASE0_BULK)).unwrap().export;
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
    let export = parse_export(&read(PHASE0_BULK)).unwrap().export;
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
    let rec = parse_export(&read(PHASE0_BODY)).unwrap().export.sessions.remove(0);
    let body = rec.body.expect("with-body payload has a body array");
    assert!(!body.is_empty());
    assert!(body.iter().any(|m| m.role.as_deref() == Some("user")));
    assert!(!rec.body_truncated);
    assert_eq!(rec.body_error, None);
}

// ---- harvest-completion Phase 1 (docs/design/2026-07-20-harvest-completion.md):
// the Phase 0 spike test, extended from RED to GREEN. Phase 0 asserted only on
// fields that were already `Option<...>` (so the file compiled against the
// pre-Phase-1 non-Option `cwd`/`created`/`title`/`first_prompt` contract) and
// panicked if the batch parse failed. Phase 1 relaxes those four fields to
// present-null `Option<String>` and switches `parse_export` to per-record
// deserialize, so this test now asserts all four deserialize to `None` (the
// assertions Phase 0's note deferred). Reverting any of those four fields to a
// plain `String` turns this test RED - the regression bite.
#[test]
fn parse_tolerates_null_string_fields() {
    // `9b17cdba-...` in this real export carries `cwd`, `created`, `title`,
    // and `first-prompt` all as JSON `null` (a genuinely empty/never-touched
    // session, `n-msgs: 0`), alongside real rows where only some of those
    // fields are null. Post-Phase-1 all four are `Option<String>`, so the whole
    // batch parses cleanly instead of aborting on clyde's production error
    // ("invalid type: null, expected a string").
    let export = parse_export(&read(NULL_STRING_FIELDS))
        .unwrap_or_else(|err| {
            panic!(
                "parse_export must tolerate null cwd/created/title/first-prompt \
                 (locks the harvest-completion Problem #1 bug); failed with: {err:#}"
            )
        })
        .export;
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
    // The four newly-relaxed fields: all present-null -> None (the Phase 0
    // deferred assertions, now live - reverting a field to `String` won't
    // compile this `None`).
    assert_eq!(empty_bomb.cwd, None);
    assert_eq!(empty_bomb.created, None);
    assert_eq!(empty_bomb.title, None);
    assert_eq!(empty_bomb.first_prompt, None);
    // Already-Option null classes ride along in the same real record.
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
    assert_eq!(
        work_session.title.as_deref(),
        Some("Review Slack export script for security vulnerabilities")
    );
    assert!(work_session.created.is_some(), "a real session has a non-null created");
    assert_eq!(work_session.model, None);

    // A real `--id --with-body` export where clyde itself reports an empty
    // transcript (`body: null`, `body-error: "parsed empty"`) - its
    // title/cwd/created are null too and it parses cleanly post-Phase-1.
    let body_export = parse_export(&read(EMPTY_BODY))
        .unwrap_or_else(|err| panic!("empty-body export must parse: {err:#}"))
        .export;
    let empty_session = &body_export.sessions[0];
    assert_eq!(empty_session.cwd, None);
    assert_eq!(empty_session.title, None);
    assert_eq!(empty_session.body, None);
    assert_eq!(empty_session.body_error.as_deref(), Some("parsed empty"));
}

// ---- harvest-completion Phase 1: per-record parse resilience. The Phase 0
// spike asserted `parse_export(MALFORMED_RECORD).is_err()` (the whole-batch
// parser aborted on one wrong-typed element). Phase 1 INVERTS that: the
// malformed element is SKIPPED and carried out as a `ParseRejection` while the
// rest of the batch parses. Reverting to whole-batch parsing turns this RED.
#[test]
fn malformed_record_is_skipped_and_the_rest_parse() {
    let parsed = parse_export(&read(MALFORMED_RECORD)).expect("a malformed element must not abort the batch");
    // The good companion (`28b526fb`, `n-msgs: 3`) survives; the synthetic twin
    // (`malformed-...`, `n-msgs: "not-a-number"`) is skipped.
    assert_eq!(parsed.export.sessions.len(), 1, "the well-formed record still parses");
    assert_eq!(
        parsed.export.sessions[0].session_id,
        "28b526fb-7061-477d-8399-bf310671d6b5"
    );
    // Exactly one parse rejection, keyed by the session-id recovered from the
    // malformed element via `serde_json::Value` (always present in the contract).
    assert_eq!(parsed.rejections.len(), 1, "one skipped malformed record");
    let rej = &parsed.rejections[0];
    assert_eq!(
        rej.session_id.as_deref(),
        Some("malformed-00000000-0000-0000-0000-000000000000"),
        "session-id recovered even from a malformed record"
    );
    assert!(
        rej.reason.contains("malformed session record"),
        "reason carries the serde error: {}",
        rej.reason
    );
    // The envelope-level fields (cursor) still parse.
    assert_eq!(parsed.export.cursor, 1495);
}

#[test]
fn unreadable_session_id_falls_back_to_element_index() {
    // A malformed element whose `session-id` itself is unreadable (wrong type)
    // still yields a durable, non-anonymous rejection keyed by element index.
    let payload = br#"{
        "schema-version": 1,
        "cursor": 7,
        "sessions": [
            {"session-id": 12345, "host": "desk", "scope": "work", "modified": "x", "dormant": true, "n-msgs": 1}
        ]
    }"#;
    let parsed = parse_export(payload).expect("envelope parses; the element is skipped");
    assert_eq!(parsed.export.sessions.len(), 0);
    assert_eq!(parsed.rejections.len(), 1);
    assert_eq!(parsed.rejections[0].session_id, None, "unreadable id -> None");
    assert_eq!(parsed.rejections[0].index, 0, "keyed by element index instead");
}

#[test]
fn schema_version_mismatch_is_a_loud_error() {
    // 3 is the next UNRELEASED version: this test pins the guard's behavior, so
    // it must name a version harvest does not speak, not whatever clyde ships.
    let payload = br#"{"schema-version": 3, "cursor": 1, "sessions": []}"#;
    let err = parse_export(payload).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("unsupported schema-version 3"), "got: {msg}");
}

#[test]
fn both_reviewed_versions_are_accepted() {
    // The v2 change touches only `scope`, which harvest never reads, so both
    // versions parse. The captured v1 fixtures depend on this.
    for v in [1, 2] {
        let payload = format!(r#"{{"schema-version": {v}, "cursor": 1, "sessions": []}}"#);
        assert!(
            parse_export(payload.as_bytes()).is_ok(),
            "schema-version {v} must be accepted"
        );
    }
}

#[test]
fn a_live_shaped_v2_record_parses() {
    // Guards the 1 -> 2 bump with the field set clyde v0.25.3 actually emits,
    // including the keys v2 added, which the tolerant envelope must ignore.
    let payload = br#"{"schema-version": 2, "cursor": 1, "sessions": [{
        "session-id": "abc-123", "host": "desk", "scope": "work",
        "cwd": "/home/x", "project-dir": "/home/x/.claude", "repo": "o/r",
        "git-branch": "main", "created": "2026-08-01T00:00:00Z",
        "modified": "2026-08-02T00:00:00Z", "updated-at": 5, "duration-secs": 60,
        "dormant": true, "title": "t", "first-prompt": "p", "n-msgs": 12,
        "model": "claude-sonnet-5", "summary": "s", "tags": ["rust"],
        "enrich-status": "ok", "redaction-count": 0,
        "transcript-path": "/t.jsonl", "staged-path": null, "archived": false,
        "efficiency": {"cache-reuse": 0.5}, "scope-version": 2,
        "prompt-version": 7, "enrich-model": "claude-sonnet-5",
        "enriched-at": "2026-08-02T00:00:00Z", "tags-source": "enrich"
    }]}"#;
    let parsed = parse_export(payload).expect("v2 payload must parse");
    assert_eq!(
        parsed.rejections.len(),
        0,
        "no field may be rejected: {:?}",
        parsed.rejections
    );
    assert_eq!(parsed.export.sessions.len(), 1);
    assert_eq!(parsed.export.sessions[0].session_id, "abc-123");
    assert_eq!(parsed.export.sessions[0].scope, "work");
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

// ---- harvest-completion Phase 6: gap-fill for `BodyMessage.role`/`text`, the
// one Phase-1-relaxed nullable pair that Phase 0/1 never asserted a direct
// `None` deserialization for (only the already-non-null golden fixture body
// was exercised, `with_body_payload_parses_body_array`). Reverting either
// field's `Option<String>` back to a plain `String` is a compile error against
// this test's `assert_eq!(..., None)` calls - a stronger bite than a runtime
// RED, since it blocks the whole crate from building.
#[test]
fn body_message_role_and_text_are_present_null_tolerant() {
    let msg: BodyMessage = serde_json::from_value(serde_json::json!({
        "role": null, "text": null, "subagent": false
    }))
    .unwrap_or_else(|e| panic!("BodyMessage must tolerate present-null role/text: {e}"));
    assert_eq!(msg.role, None);
    assert_eq!(msg.text, None);
    assert!(!msg.subagent);

    // Omitted (not just present-null) also degrades to None via `#[serde(default)]`.
    let omitted: BodyMessage = serde_json::from_value(serde_json::json!({}))
        .unwrap_or_else(|e| panic!("BodyMessage must tolerate a wholly-omitted role/text/subagent: {e}"));
    assert_eq!(omitted.role, None);
    assert_eq!(omitted.text, None);
    assert!(!omitted.subagent, "subagent defaults false when omitted");
}
