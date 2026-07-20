#![allow(clippy::unwrap_used)]
use super::*;
use tempfile::TempDir;

use crate::config::Config;
use crate::harvest::contract::{BodyMessage, EnrichStatus, ParsedExport, SessionExport, SessionRecord};
use crate::harvest::select::SelectionConfig;
use crate::harvest::watermark::WatermarkState;
use crate::harvest::{HarvestOpts, plan_harvest};
use crate::receipts;
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};

// Env-var mutation isn't safe under parallel tests (rust.md "Platform path
// testing"): serialize every test that points XDG_DATA_HOME at a tempdir. The
// ONE shared lock (`crate::harvest::TEST_XDG_LOCK`) also serializes against
// `harvest::tests`'s live-run test, which redirects the same env var - a
// per-file lock would let those two race the receipts DB.
use crate::harvest::TEST_XDG_LOCK as ENV_LOCK;

fn session_record(id: &str, created: &str, modified: &str, n_msgs: i64) -> SessionRecord {
    SessionRecord {
        session_id: id.to_string(),
        host: "desk".to_string(),
        scope: "work".to_string(),
        cwd: Some("/home/saidler/repos/tatari-tv/slack-cli/main".to_string()),
        project_dir: None,
        repo: Some("tatari-tv/slack-cli".to_string()),
        git_branch: Some("main".to_string()),
        created: Some(created.to_string()),
        modified: modified.to_string(),
        updated_at: None,
        duration_secs: Some(600),
        dormant: true,
        title: Some(format!("session {id}")),
        first_prompt: Some("do the thing".to_string()),
        n_msgs,
        model: None,
        summary: None,
        tags: vec![],
        enrich_status: Some(EnrichStatus::Ok),
        redaction_count: 0,
        transcript_path: None,
        staged_path: None,
        archived: false,
        repos_touched: None,
        files_touched: None,
        body: None,
        body_truncated: false,
        body_error: None,
    }
}

/// In-memory reader: bulk export is fixed; `export_with_body` looks up the
/// matching bulk record and attaches the configured body.
struct FakeReader {
    bulk: SessionExport,
    bodies: std::collections::BTreeMap<String, Vec<BodyMessage>>,
}

impl ExportReader for FakeReader {
    async fn export_bulk(
        &self,
        _cursor: Option<i64>,
        _since: Option<&str>,
        _limit: Option<usize>,
    ) -> eyre::Result<ParsedExport> {
        Ok(ParsedExport {
            export: self.bulk.clone(),
            rejections: Vec::new(),
        })
    }

    async fn export_with_body(&self, id: &str) -> eyre::Result<SessionRecord> {
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

fn opts() -> HarvestOpts {
    HarvestOpts {
        selection: SelectionConfig::compile(6, &[]).unwrap(),
        thread_window: chrono::Duration::hours(2),
        force: false,
    }
}

fn test_config(vault_root: &std::path::Path, staging_root: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.vault.root_path = Some(vault_root.to_string_lossy().to_string());
    config.staging.enabled = true;
    config.staging.root = staging_root.to_path_buf();
    config.fabric.binary = "borg-test-fabric-binary-does-not-exist".to_string();
    // See the identical comment in `pipeline::session::tests::test_config`:
    // `finalize_tags` caches the loaded canonical vocabulary process-wide on
    // first success, so this must NOT resolve to the real
    // `~/.config/sb/canonical-tags.yml` (would poison every other test in
    // this binary with this machine's real tag catalogue). Guaranteed-absent
    // paths make the load fail and no-op instead.
    config.tags.canonical_path = staging_root.join("no-such-canonical-tags.yml").display().to_string();
    config.tags.mapping_path = staging_root.join("no-such-tag-mapping.yml").display().to_string();
    config
}

/// End-to-end (reusing the 2026-07-02 golden-fixture session ids/timestamps,
/// Phase 3's checked-in slack-cli work pair): plan -> publish lands ONE
/// `inbox/` note for the clustered thread, the receipts row transitions
/// `received -> succeeded`, and `record_published` writes a snapshot that
/// makes an immediate rerun a no-op (Phase 3 watermark idempotency, tied in
/// end to end).
#[tokio::test]
async fn publish_plan_publishes_and_rerun_is_idempotent() {
    let _guard = ENV_LOCK.lock().await;
    // `process_content` panics if the permit pools were never sized (a
    // programmer error normally caught by `serve_init`/CLI startup); this
    // test drives `process_content` directly, so it stands in for that
    // startup step. `init` is idempotent (a second call across test files is
    // a harmless no-op warn), so this is safe under parallel test execution.
    crate::pipeline::permits::GENERAL_PERMITS.init(4);
    crate::pipeline::permits::HEAVY_PERMITS.init(2);
    let data_home = TempDir::new().unwrap();
    let prior_xdg = std::env::var("XDG_DATA_HOME").ok();
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };

    let vault_dir = TempDir::new().unwrap();
    let staging_dir = TempDir::new().unwrap();
    let config = test_config(vault_dir.path(), staging_dir.path());

    let s1 = session_record(
        "871f6428",
        "2026-07-02T04:51:21+00:00",
        "2026-07-02T06:08:39+00:00",
        486,
    );
    let s2 = session_record(
        "4ae69e3a",
        "2026-07-02T06:08:54+00:00",
        "2026-07-02T06:20:00+00:00",
        320,
    );
    let export = SessionExport {
        schema_version: 1,
        generated_at: None,
        host: None,
        cursor: 42,
        sessions: vec![s1, s2],
    };
    let reader = FakeReader {
        bulk: export.clone(),
        bodies: [
            (
                "871f6428".to_string(),
                vec![BodyMessage {
                    role: Some("human".to_string()),
                    text: Some("let's build the thing".to_string()),
                    subagent: false,
                }],
            ),
            (
                "4ae69e3a".to_string(),
                vec![BodyMessage {
                    role: Some("assistant".to_string()),
                    text: Some("here's a plan".to_string()),
                    subagent: false,
                }],
            ),
        ]
        .into_iter()
        .collect(),
    };

    let state = WatermarkState::default();
    let plan = plan_harvest(&reader, export.clone(), &opts(), &state).await.unwrap();
    assert_eq!(
        plan.threads.len(),
        1,
        "same-cwd/branch pair within the window clusters to one thread"
    );
    assert_eq!(plan.threads[0].member_ids.len(), 2);
    assert_eq!(plan.publishable().count(), 1);

    let (state, outcomes) = publish_plan(&reader, &config, &plan, state).await;
    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0].result.status, crate::types::IngestStatus::Completed),
        "{:?}",
        outcomes[0].result
    );
    assert!(
        state.published.contains_key("871f6428"),
        "primary id recorded in the watermark"
    );
    let published = &state.published["871f6428"];
    assert_eq!(published.n_msgs, 806);

    // Receipts row transitioned received -> succeeded.
    let conn = receipts::open_default().unwrap();
    let row = receipts::get(&conn, &outcomes[0].trace_id)
        .unwrap()
        .expect("receipts row");
    assert_eq!(row.status, "succeeded");
    assert_eq!(row.kind, "session");
    assert!(row.note_path.is_some());

    // Rerun on the SAME export with the updated state: n-msgs unchanged ->
    // the cheap filter skips without a body fetch or re-distill.
    let plan2 = plan_harvest(&reader, export, &opts(), &state).await.unwrap();
    assert_eq!(
        plan2.publishable().count(),
        0,
        "an unchanged rerun must not re-select the already-published thread"
    );

    // Phase 7: publish staged members.yml, and `replay --from-stage 2`
    // re-derives the note from the staged transcript + members WITHOUT
    // touching clyde. Structurally equivalent: the trace re-publishes
    // successfully (same source:/trace:, valid Distilled) - byte identity is
    // not asserted over an (here degraded) distill pass.
    let trace_id = outcomes[0].trace_id.clone();
    let store = FsArtifactStore::from_config(&config.staging);
    assert!(
        store
            .read_attachment(&trace_id, crate::harvest::SESSION_REPLAY_META_FILE)
            .unwrap()
            .is_some(),
        "publish stages members.yml for stage-2 replay"
    );
    let replay_opts = crate::replay::ReplayOptions {
        trace_id: Some(trace_id.clone()),
        from_stage: 2,
        ..Default::default()
    };
    let report = crate::replay::run(config, replay_opts, |_| {}).await.unwrap();
    assert_eq!(report.succeeded, 1, "stage-2 replay re-derives the session note");
    assert_eq!(report.failed, 0);

    match prior_xdg {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }
}
