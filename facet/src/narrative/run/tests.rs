use super::*;
use crate::config::Config;
use crate::fabric::FakeFabric;
use crate::gems::{InteractionTurn, Review};
use crate::ledger::gems::NewGem;
use crate::ledger::workitems::NewWorkItem;
use chrono::{TimeZone, Utc};

fn ts(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).single().expect("ts")
}

fn ledger_with_session_gems(session: &str, with_obstacle: bool, count: usize) -> (Ledger, i64) {
    let l = Ledger::open_in_memory().expect("ledger");
    l.apply_facet_v2_schema().expect("schema");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "wi",
            title: "wi",
            created_at: Utc::now(),
        })
        .expect("workitem");
    for i in 0..count {
        let tag = if with_obstacle && i == 0 { "name-the-failure" } else { "frame" };
        let turns = vec![
            InteractionTurn {
                ai_says: format!("ai {i}"),
                ai_turn_uuid: format!("ai-{session}-{i}"),
                user_says: format!("user {i}"),
                user_turn_uuid: format!("u-{session}-{i}"),
                tags: vec![tag.to_string()],
            },
            InteractionTurn {
                ai_says: format!("ai2 {i}"),
                ai_turn_uuid: format!("ai2-{session}-{i}"),
                user_says: format!("ack {i}"),
                user_turn_uuid: format!("u2-{session}-{i}"),
                tags: vec!["verify".to_string()],
            },
        ];
        l.upsert_gem(NewGem {
            workitem_id: wid,
            session_uuid: session,
            task: &format!("task {i}"),
            context_loaded: &[],
            context_missing: &[],
            interaction: &turns,
            review: &Review::default(),
            tags: &[tag.to_string()],
            why_it_matters: "matters",
            extractor_model: "sonnet",
            extracted_at: ts(2026, 5, 1, 10 + i as u32),
        })
        .expect("gem");
    }
    (l, wid)
}

struct ConstEmbedder;
impl Embedder for ConstEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![1.0, 0.0])
    }
}

fn narrate_response_accept() -> &'static str {
    "{\"title\":\"Three Things\",\"thesis\":\"thesis here\",\"body_md\":\"body\",\"gem_ids\":[],\"chronologically_ordered\":true}"
}

fn narrate_response_skip() -> &'static str {
    "{\"title\":\"\",\"thesis\":\"\",\"body_md\":\"\",\"gem_ids\":[],\"chronologically_ordered\":true}"
}

#[tokio::test]
async fn session_arc_synthesises_when_obstacle_present() {
    let (l, _wid) = ledger_with_session_gems("s1", true, 3);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-narrate", narrate_response_accept());
    let report = run_with_fabric(
        &cfg,
        &l,
        tmp.path(),
        ArchetypeFilter::Only(Archetype::Session),
        &fabric,
        &ConstEmbedder,
    )
    .await
    .expect("run");
    assert_eq!(report.candidates_considered, 1);
    assert_eq!(report.narratives_synthesised, 1);
}

#[tokio::test]
async fn session_arc_skips_when_no_obstacle() {
    let (l, _wid) = ledger_with_session_gems("s1", false, 3);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    let report = run_with_fabric(
        &cfg,
        &l,
        tmp.path(),
        ArchetypeFilter::Only(Archetype::Session),
        &fabric,
        &ConstEmbedder,
    )
    .await
    .expect("run");
    // No obstacle -> no candidate emitted by discover_session_arcs.
    assert_eq!(report.candidates_considered, 0);
}

#[tokio::test]
async fn rejection_gate_counted_as_skipped_not_synthesised() {
    let (l, _wid) = ledger_with_session_gems("s1", true, 3);
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-narrate", narrate_response_skip());
    let report = run_with_fabric(
        &cfg,
        &l,
        tmp.path(),
        ArchetypeFilter::Only(Archetype::Session),
        &fabric,
        &ConstEmbedder,
    )
    .await
    .expect("run");
    assert_eq!(report.narratives_synthesised, 0);
    assert_eq!(report.narratives_skipped_by_gate, 1);
}

#[tokio::test]
async fn rejected_spectrum_suppresses_overlapping_candidate() {
    let (l, _wid) = ledger_with_session_gems("s1", true, 3);
    let tmp = tempfile::tempdir().expect("tempdir");
    // Drop a rejected spectrum note covering gem ids 1, 2, 3 (full overlap).
    let spectra_dir = tmp.path().join("notes/facet/spectra");
    std::fs::create_dir_all(&spectra_dir).expect("mkdir");
    let rejected_md = "---\nfacet-spectrum-status: rejected\nfacet-spectrum-cluster-key: s1\nfacet-spectrum-archetype: session\nfacet-spectrum-gem-ids:\n- 1\n- 2\n- 3\n---\n\n# rejected\n";
    std::fs::write(spectra_dir.join("rejected.md"), rejected_md).expect("write");

    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-narrate", narrate_response_accept());
    let report = run_with_fabric(
        &cfg,
        &l,
        tmp.path(),
        ArchetypeFilter::Only(Archetype::Session),
        &fabric,
        &ConstEmbedder,
    )
    .await
    .expect("run");
    assert_eq!(report.candidates_considered, 1);
    assert_eq!(report.candidates_suppressed_by_rejection, 1);
    assert_eq!(report.narratives_synthesised, 0);
}
