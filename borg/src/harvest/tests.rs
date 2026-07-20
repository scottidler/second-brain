#![allow(clippy::unwrap_used)]
use super::*;
use std::sync::Mutex;
use tempfile::TempDir;

use contract::{BodyMessage, ParseRejection, ParsedExport, SessionRecord, parse_export};
use watermark::{Reappearance, body_hash, thread_body_text};

use crate::config::StagingLayout;
use crate::stages::artifact::FsArtifactStore;

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/harvest/golden-2026-07-02.json"
);
const SAME_CWD: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/harvest/same-cwd-unrelated.json"
);
const REJECTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../config/eval/harvest/reject-cases.json");
const SINGLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/eval/harvest/single-repo-session.json"
);

fn load(path: &str) -> SessionExport {
    parse_export(&std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}")))
        .unwrap()
        .export
}

/// In-memory reader so selection/clustering/watermark logic runs without the
/// clyde binary. Records every `export_with_body` id so tests can assert a
/// Skip fetched NO body ("skip WITHOUT re-distilling"). `parse_rejections` are
/// carried out of `export_bulk` alongside the good records, exactly as the real
/// per-record parse boundary does.
struct FakeReader {
    bulk: SessionExport,
    parse_rejections: Vec<ParseRejection>,
    bodies: std::collections::BTreeMap<String, Vec<BodyMessage>>,
    with_body_calls: Mutex<Vec<String>>,
}

impl FakeReader {
    fn new(bulk: SessionExport) -> Self {
        Self {
            bulk,
            parse_rejections: Vec::new(),
            bodies: std::collections::BTreeMap::new(),
            with_body_calls: Mutex::new(Vec::new()),
        }
    }

    fn with_parse_rejections(mut self, rejections: Vec<ParseRejection>) -> Self {
        self.parse_rejections = rejections;
        self
    }

    fn with_body(mut self, id: &str, body: Vec<BodyMessage>) -> Self {
        self.bodies.insert(id.to_string(), body);
        self
    }

    fn body_fetch_count(&self) -> usize {
        self.with_body_calls.lock().unwrap().len()
    }
}

impl reader::ExportReader for FakeReader {
    async fn export_bulk(
        &self,
        _cursor: Option<i64>,
        _since: Option<&str>,
        _limit: Option<usize>,
    ) -> eyre::Result<ParsedExport> {
        Ok(ParsedExport {
            export: self.bulk.clone(),
            rejections: self.parse_rejections.clone(),
        })
    }

    async fn export_with_body(&self, id: &str) -> eyre::Result<SessionRecord> {
        self.with_body_calls.lock().unwrap().push(id.to_string());
        let mut rec = self
            .bulk
            .sessions
            .iter()
            .find(|s| s.session_id == id)
            .cloned()
            .unwrap_or_else(|| panic!("fake reader has no record for {id}"));
        rec.body = Some(self.bodies.get(id).cloned().unwrap_or_default());
        Ok(rec)
    }
}

fn opts(min_msgs: usize, patterns: &[&str], force: bool) -> HarvestOpts {
    HarvestOpts {
        selection: SelectionConfig::compile(min_msgs, &patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap(),
        thread_window: Duration::hours(2),
        force,
    }
}

fn msg(text: &str) -> Vec<BodyMessage> {
    vec![BodyMessage {
        role: Some("user".into()),
        text: Some(text.into()),
        subagent: false,
    }]
}

// ---- Golden fixture (success criterion): exact ids, cluster count, note count.

#[tokio::test]
async fn golden_fixture_selects_expected_ids_and_one_note() {
    let export = load(GOLDEN);
    let reader = FakeReader::new(export.clone());
    let plan = plan_harvest(&reader, export, &opts(6, &[], false), &WatermarkState::default())
        .await
        .unwrap();

    // Exactly one thread / one note.
    assert_eq!(plan.threads.len(), 1, "golden arc -> 1 note");
    let thread = &plan.threads[0];
    assert_eq!(
        thread.member_ids,
        vec![
            "871f6428-92d8-4035-a66c-87f6d1edee83".to_string(),
            "4ae69e3a-6bde-47d3-946d-c9757f810610".to_string(),
        ]
    );
    assert_eq!(thread.primary_id, "871f6428-92d8-4035-a66c-87f6d1edee83");
    assert_eq!(thread.total_msgs, 806);
    assert_eq!(thread.decision, Reappearance::NewNote);

    // The two personal sessions are rejected (skipped-personal / non-repo cwd).
    let mut rejected: Vec<&str> = plan.rejections.iter().map(|r| r.session_id.as_str()).collect();
    rejected.sort();
    assert_eq!(
        rejected,
        vec![
            "4e55a52c-f0be-40eb-88a7-3184c7640738",
            "9521f589-1243-4264-8302-ce28d9e524ff",
        ]
    );
    for r in &plan.rejections {
        assert!(r.record.reason.contains("skipped-personal"), "{}", r.record.reason);
    }
    assert_eq!(plan.new_cursor, 1500);
    // NewNote never fetches a body (that is Phase 5's publish step).
    assert_eq!(reader.body_fetch_count(), 0);
}

// ---- Same-cwd-unrelated (success criterion): must NOT merge.

#[tokio::test]
async fn same_cwd_unrelated_does_not_merge() {
    let export = load(SAME_CWD);
    let reader = FakeReader::new(export.clone());
    let plan = plan_harvest(&reader, export, &opts(6, &[], false), &WatermarkState::default())
        .await
        .unwrap();
    assert_eq!(plan.threads.len(), 2, "sessions >2h apart stay separate notes");
    assert_eq!(plan.rejections.len(), 0);
}

// ---- Rejects (success criterion): rejection.yml + rejected receipts row.

#[tokio::test]
async fn reject_cases_each_fail_their_own_gate() {
    let export = load(REJECTS);
    let reader = FakeReader::new(export.clone());
    let plan = plan_harvest(
        &reader,
        export,
        &opts(6, &["security-review"], false),
        &WatermarkState::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan.threads.len(), 0);
    assert_eq!(plan.rejections.len(), 5);

    let reason_for = |suffix: &str| {
        plan.rejections
            .iter()
            .find(|r| r.session_id.ends_with(suffix))
            .map(|r| r.record.reason.clone())
            .unwrap_or_else(|| panic!("no rejection ending {suffix}"))
    };
    assert!(reason_for("dorm").contains("not dormant"));
    assert!(reason_for("perso").contains("skipped-personal"));
    assert!(reason_for("norepo").contains("is not a repo"));
    assert!(reason_for("belowbar").contains("below message threshold"));
    assert!(reason_for("excludedp").contains("excluded by pattern"));
}

#[tokio::test]
async fn write_rejections_leaves_yaml_and_a_rejected_receipts_row() {
    let export = load(REJECTS);
    let reader = FakeReader::new(export.clone());
    let plan = plan_harvest(
        &reader,
        export,
        &opts(6, &["security-review"], false),
        &WatermarkState::default(),
    )
    .await
    .unwrap();

    let staging = TempDir::new().unwrap();
    let store = FsArtifactStore::new(staging.path(), StagingLayout::PerTrace);
    let conn = crate::receipts::open_memory().unwrap();

    write_rejections(&store, &conn, &plan.rejections).unwrap();

    for rej in &plan.rejections {
        // rejection.yml forensic artifact, keyed by the selection-time trace.
        let read_back = store.read_rejection(&rej.trace_id).unwrap();
        let record = read_back.unwrap_or_else(|| panic!("no rejection.yml for {}", rej.trace_id));
        assert_eq!(record.gate, crate::types::GateId::Selection);
        assert_eq!(record.trace, rej.trace_id);
    }

    // Every reject is a `rejected` receipts row keyed by its trace.
    let rejected = crate::receipts::query(
        &conn,
        &crate::receipts::Filter {
            status: Some(vault::receipts::ReceiptStatus::Rejected),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rejected.len(), plan.rejections.len());
    for row in &rejected {
        assert_eq!(row.kind, "session", "an honest session kind, never lying as text/url");
        assert!(row.failure_reason.is_some());
    }
}

// ---- Watermark: rerun with unchanged catalog is a no-op.

#[tokio::test]
async fn rerun_with_unchanged_catalog_is_a_no_op() {
    let export = load(SINGLE);
    let primary = "2dda6936-3fb5-47bc-9c88-9306416bdb11";
    let body = msg("the original transcript");

    // First run: NewNote.
    let reader = FakeReader::new(export.clone()).with_body(primary, body.clone());
    let plan1 = plan_harvest(
        &reader,
        export.clone(),
        &opts(6, &[], false),
        &WatermarkState::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan1.threads[0].decision, Reappearance::NewNote);

    // Simulate Phase 5 publish: advance cursor + record the snapshot.
    let mut state = apply_plan_to_state(WatermarkState::default(), &plan1);
    let published_hash = body_hash(&thread_body_text(&[(primary.to_string(), body.clone())]));
    record_published(
        &mut state,
        primary,
        "inbox/2dda6936.md",
        plan1.threads[0].total_msgs,
        &published_hash,
    );

    // Second run, identical catalog + a reader that would PANIC-track any body
    // fetch: n-msgs unchanged -> cheap-filter Skip, no fetch, nothing publishable.
    let reader2 = FakeReader::new(export.clone()).with_body(primary, body.clone());
    let plan2 = plan_harvest(&reader2, export, &opts(6, &[], false), &state)
        .await
        .unwrap();
    assert_eq!(plan2.threads[0].decision, Reappearance::Skip { snapshot_update: None });
    assert_eq!(reader2.body_fetch_count(), 0, "cheap filter must not fetch a body");
    assert_eq!(plan2.publishable().count(), 0, "unchanged rerun lands no note");
}

// ---- Watermark: resumed session (body hash changed) -> follow-up.

#[tokio::test]
async fn resumed_session_body_hash_changed_is_follow_up() {
    let export = load(SINGLE);
    let primary = "2dda6936-3fb5-47bc-9c88-9306416bdb11";
    let original = msg("the original transcript");

    // Seed a published snapshot at 70 msgs / original hash.
    let mut state = WatermarkState {
        cursor: Some(1800),
        ..Default::default()
    };
    let original_hash = body_hash(&thread_body_text(&[(primary.to_string(), original.clone())]));
    record_published(&mut state, primary, "inbox/2dda6936.md", 70, &original_hash);

    // Re-appear with grown n-msgs AND a different body (a resume).
    let mut grown = export.clone();
    grown.sessions[0].n_msgs = 90;
    let resumed_body = msg("the original transcript PLUS a resumed follow-up turn");
    let reader = FakeReader::new(grown.clone()).with_body(primary, resumed_body);

    let plan = plan_harvest(&reader, grown, &opts(6, &[], false), &state)
        .await
        .unwrap();
    assert!(matches!(plan.threads[0].decision, Reappearance::FollowUp { .. }));
    assert_eq!(
        reader.body_fetch_count(),
        1,
        "changed n-msgs triggers the deep-check fetch"
    );
    assert_eq!(plan.publishable().count(), 1, "a follow-up note lands");
}

// ---- Watermark: unchanged body but grown n-msgs -> Skip WITHOUT re-distilling.

#[tokio::test]
async fn unchanged_body_skips_and_advances_without_redistilling() {
    let export = load(SINGLE);
    let primary = "2dda6936-3fb5-47bc-9c88-9306416bdb11";
    let body = msg("the original transcript");

    let mut state = WatermarkState {
        cursor: Some(1800),
        ..Default::default()
    };
    let hash = body_hash(&thread_body_text(&[(primary.to_string(), body.clone())]));
    record_published(&mut state, primary, "inbox/2dda6936.md", 70, &hash);

    // n-msgs grew (metadata churn) but the body is byte-identical.
    let mut grown = export.clone();
    grown.sessions[0].n_msgs = 90;
    let reader = FakeReader::new(grown.clone()).with_body(primary, body.clone());

    let plan = plan_harvest(&reader, grown, &opts(6, &[], false), &state)
        .await
        .unwrap();
    match &plan.threads[0].decision {
        Reappearance::Skip {
            snapshot_update: Some(e),
        } => assert_eq!(e.n_msgs, 90),
        other => panic!("expected Skip with snapshot advance, got {other:?}"),
    }
    assert_eq!(
        plan.publishable().count(),
        0,
        "no re-distill: an unchanged body never re-publishes"
    );

    // Apply the advance; a third run cheap-filters (90 == 90), no fetch.
    let state2 = apply_plan_to_state(state, &plan);
    assert_eq!(state2.published[primary].n_msgs, 90);
    let mut grown2 = export.clone();
    grown2.sessions[0].n_msgs = 90;
    let reader2 = FakeReader::new(grown2.clone()).with_body(primary, body);
    let plan3 = plan_harvest(&reader2, grown2, &opts(6, &[], false), &state2)
        .await
        .unwrap();
    assert_eq!(plan3.threads[0].decision, Reappearance::Skip { snapshot_update: None });
    assert_eq!(
        reader2.body_fetch_count(),
        0,
        "advanced snapshot -> cheap filter, never re-fetches"
    );
}

// ---- Force re-distills a published, unchanged session.

#[tokio::test]
async fn force_redistills_published_session() {
    let export = load(SINGLE);
    let primary = "2dda6936-3fb5-47bc-9c88-9306416bdb11";
    let body = msg("the original transcript");
    let mut state = WatermarkState::default();
    let hash = body_hash(&thread_body_text(&[(primary.to_string(), body.clone())]));
    record_published(&mut state, primary, "inbox/2dda6936.md", 70, &hash);

    let reader = FakeReader::new(export.clone()).with_body(primary, body);
    let plan = plan_harvest(&reader, export, &opts(6, &[], true), &state)
        .await
        .unwrap();
    assert!(matches!(plan.threads[0].decision, Reappearance::FollowUp { .. }));
    assert_eq!(
        reader.body_fetch_count(),
        0,
        "force needs no hash - it re-distills regardless"
    );
}

#[tokio::test]
async fn run_with_dry_run_writes_nothing_and_reports_selection() {
    // Phase 6: `sb borg harvest --dry-run` lists selections/rejections and
    // writes NOTHING. Drives the corrected deterministic golden outcome
    // (2 selected -> 1 thread note, 2 rejected) through the run core.
    let reader = FakeReader::new(load(GOLDEN));
    let mut config = crate::config::Config::default();
    config.harvest.min_msgs = 6; // the golden fixture's selection bar
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("harvest-state.json");

    let report = run_with(&reader, &config, &state_path, None, None, false, true)
        .await
        .expect("dry-run");

    assert!(report.dry_run);
    assert!(report.outcomes.is_empty(), "dry-run publishes nothing");
    assert_eq!(report.plan.publishable().count(), 1, "one thread note");
    assert_eq!(report.plan.rejections.len(), 2, "two personal/non-repo rejects");
    // Writes nothing: no state JSON, and no body fetches (a fresh catalog is
    // all NewNote, so the deep-check body-fetch path never runs at plan time).
    assert!(!state_path.exists(), "dry-run must not write the state file");
    assert_eq!(reader.body_fetch_count(), 0, "dry-run fetches no bodies");
}

#[tokio::test]
async fn dry_run_surfaces_parse_rejections_but_writes_no_receipt() {
    // Dry-run persists nothing and advances no watermark, so a parse skip is
    // WARN-only there (harvest-completion Phase 1) - the report still surfaces
    // it for the operator's soak review, but no receipt is forced.
    let reader = FakeReader::new(load(GOLDEN)).with_parse_rejections(vec![ParseRejection {
        session_id: Some("malformed-aaaa".to_string()),
        index: 3,
        reason: "malformed session record: invalid type".to_string(),
    }]);
    let mut config = crate::config::Config::default();
    config.harvest.min_msgs = 6;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("harvest-state.json");

    let report = run_with(&reader, &config, &state_path, None, None, false, true)
        .await
        .expect("dry-run");

    assert_eq!(report.parse_rejections.len(), 1, "parse skip surfaced in the report");
    assert_eq!(report.parse_rejections[0].session_id.as_deref(), Some("malformed-aaaa"));
    assert!(!state_path.exists(), "dry-run must not advance the watermark");
}

#[tokio::test]
async fn live_run_writes_parse_skip_receipt_before_cursor_advances() {
    // Success criterion: a LIVE-path parse skip lands a durable
    // `received->rejected` receipt keyed by `session_id`, written BEFORE the
    // watermark advances (harvest-completion Phase 1, Resolved Decision
    // "Per-record parse skip must be DURABLE"). The export carries ZERO good
    // records and ONE parse rejection, so nothing publishable runs - the run
    // reduces to exactly the durable-skip + cursor-advance sequence under test.
    //
    // The ONE shared XDG lock (`crate::harvest::TEST_XDG_LOCK`) serializes this
    // against `harvest::publish::tests`, which redirects the same env var - a
    // per-file lock would let the two race the receipts DB.
    let _guard = crate::harvest::TEST_XDG_LOCK.lock().await;
    let data_home = TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    let staging_dir = TempDir::new().unwrap();
    let mut config = crate::config::Config::default();
    config.staging.root = staging_dir.path().to_path_buf();

    let export = SessionExport {
        schema_version: 1,
        generated_at: None,
        host: None,
        cursor: 4242,
        sessions: Vec::new(),
    };
    let reader = FakeReader::new(export).with_parse_rejections(vec![ParseRejection {
        session_id: Some("malformed-11112222-3333-4444-5555-666677778888".to_string()),
        index: 0,
        reason: "malformed session record: invalid type: string, expected i64".to_string(),
    }]);

    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("harvest-state.json");
    assert!(!state_path.exists(), "no watermark yet");

    let report = run_with(&reader, &config, &state_path, None, None, false, false)
        .await
        .expect("live run");
    assert_eq!(report.parse_rejections.len(), 1);

    // The parse skip is a durable `received->rejected` receipt, keyed by the
    // recovered session_id (carried as `source: clyde://<id>`, matched by the
    // raw_input LIKE filter). It exists NOW - and the cursor advanced, so the
    // receipt was written before the advance (both are in the same live run;
    // the write precedes `state.save` in run_with).
    let conn = crate::receipts::open_default().unwrap();
    let rejected = crate::receipts::query(
        &conn,
        &crate::receipts::Filter {
            status: Some(vault::receipts::ReceiptStatus::Rejected),
            source_like: Some("clyde://malformed-11112222%".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        rejected.len(),
        1,
        "durable received->rejected receipt for the parse skip"
    );
    assert_eq!(rejected[0].kind, "session");
    assert!(
        rejected[0]
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("malformed session record")
    );

    // The watermark DID advance (state file written with the export cursor) -
    // proving the receipt above was persisted first (ordering is structural in
    // run_with: write_rejections precedes state.save).
    let saved = watermark::WatermarkState::load(&state_path).unwrap();
    assert_eq!(
        saved.cursor,
        Some(4242),
        "cursor advanced after the durable receipt landed"
    );

    match prior_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
}
