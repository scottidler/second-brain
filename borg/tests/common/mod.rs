//! Shared harness for the trace-keyed-replace regression guards
//! (`replay_lands_same_note.rs`, `body_hash_agrees_across_paths.rs`).
//!
//! Both guards drive the REAL harvest publish path end to end - plan -> door
//! capture -> pipeline dispatch -> staged artifacts - against an in-memory
//! clyde export, so nothing here mocks the seam under test.
//!
//! `dead_code` is allowed module-wide because Cargo compiles this file
//! separately into every integration-test binary that declares `mod common;`,
//! and each binary uses a different subset. This is the shared-test-helper
//! case, not a suppressed warning about real code.
#![allow(dead_code)]
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use tempfile::TempDir;

use borg::config::Config;
use borg::harvest::contract::{BodyMessage, EnrichStatus, ParsedExport, SessionExport, SessionRecord};
use borg::harvest::reader::ExportReader;
use borg::harvest::select::SelectionConfig;
use borg::harvest::watermark::WatermarkState;
use borg::harvest::{HarvestOpts, plan_harvest, publish};

/// Point `XDG_DATA_HOME` at a throwaway directory: the receipts DB, the
/// success ledger, and the harvest state/lock all live under it, and a test
/// must never touch the operator's real ones. Each integration-test file is
/// its own process with exactly ONE test, so no cross-test env race exists
/// here (unlike the lib tests, which share a process and a lock).
pub struct XdgSandbox {
    data_home: TempDir,
    prior: Option<String>,
}

impl XdgSandbox {
    pub fn new() -> Self {
        let data_home = TempDir::new().unwrap();
        let prior = std::env::var("XDG_DATA_HOME").ok();
        unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };
        Self { data_home, prior }
    }
}

impl Drop for XdgSandbox {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
        }
    }
}

pub fn session_record(id: &str, created: &str, modified: &str, n_msgs: i64) -> SessionRecord {
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
        title: Some(format!("session {id} work")),
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

/// In-memory export reader: the bulk page is fixed, and `export_with_body`
/// attaches the configured transcript to the matching bulk record.
pub struct FakeReader {
    pub bulk: SessionExport,
    pub bodies: BTreeMap<String, Vec<BodyMessage>>,
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

/// `fabric.binary` names a guaranteed-absent binary so the distill degrades
/// gracefully (no LLM in tests) and the note filename falls back to the
/// title-slug - which is what lets these guards inject a different slug. The
/// canonical/mapping paths are likewise guaranteed-absent so the process-wide
/// tag cache is never poisoned with this machine's real catalogue.
pub fn test_config(vault_root: &std::path::Path, staging_root: &std::path::Path) -> Config {
    let mut config = Config::default();
    config.vault.root_path = Some(vault_root.to_string_lossy().to_string());
    config.staging.enabled = true;
    config.staging.root = staging_root.to_path_buf();
    config.fabric.binary = "borg-test-fabric-binary-does-not-exist".to_string();
    config.tags.canonical_path = staging_root.join("no-such-canonical-tags.yml").display().to_string();
    config.tags.mapping_path = staging_root.join("no-such-tag-mapping.yml").display().to_string();
    config
}

/// A published single-thread harvest run: the config it ran under, the trace
/// the thread landed as, the member transcripts that were fetched, and the
/// resulting watermark state.
pub struct Published {
    pub config: Config,
    pub trace_id: String,
    pub primary_id: String,
    pub bodies: BTreeMap<String, Vec<BodyMessage>>,
    pub state: WatermarkState,
}

/// Drive one thread all the way through the real publish path.
pub async fn publish_one_thread(vault_root: &std::path::Path, staging_root: &std::path::Path) -> Published {
    // `process_content` panics if the permit pools were never sized (normally
    // done by `serve_init`/CLI startup); this stands in for that step.
    borg::pipeline::permits::GENERAL_PERMITS.init(4);
    borg::pipeline::permits::HEAVY_PERMITS.init(2);

    let config = test_config(vault_root, staging_root);
    let primary_id = "871f6428";
    let export = SessionExport {
        schema_version: 1,
        generated_at: None,
        host: None,
        cursor: 42,
        sessions: vec![session_record(
            primary_id,
            "2026-07-02T04:51:21+00:00",
            "2026-07-02T06:08:39+00:00",
            486,
        )],
    };
    let bodies: BTreeMap<String, Vec<BodyMessage>> = [(
        primary_id.to_string(),
        vec![
            BodyMessage {
                role: Some("human".to_string()),
                text: Some("migrate ci.yml to the reusable workflow".to_string()),
                subagent: false,
            },
            BodyMessage {
                role: Some("assistant".to_string()),
                text: Some("here is the plan".to_string()),
                subagent: false,
            },
        ],
    )]
    .into_iter()
    .collect();
    let reader = FakeReader {
        bulk: export.clone(),
        bodies: bodies.clone(),
    };

    let opts = HarvestOpts {
        selection: SelectionConfig::compile(6, &[]).unwrap(),
        thread_window: chrono::Duration::hours(2),
        force: false,
    };
    let plan = plan_harvest(&reader, export, &opts, &WatermarkState::default())
        .await
        .unwrap();
    assert_eq!(plan.publishable().count(), 1);
    let trace_id = plan.threads[0].trace_id.clone();

    let state_path = staging_root.join("harvest-state.json");
    let (state, outcomes) =
        publish::publish_plan(&reader, &config, &plan, WatermarkState::default(), &state_path).await;
    assert!(
        matches!(outcomes[0].result.status, borg::types::IngestStatus::Completed),
        "{:?}",
        outcomes[0].result
    );

    Published {
        config,
        trace_id,
        primary_id: primary_id.to_string(),
        bodies,
        state,
    }
}
