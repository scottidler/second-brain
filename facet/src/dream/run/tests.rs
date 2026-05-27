use super::*;
use crate::Ledger;
use crate::gems::{InteractionTurn, Review};
use crate::ledger::gems::NewGem;
use crate::ledger::workitems::NewWorkItem;
use chrono::{TimeZone, Utc};

fn ts(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).single().expect("ts")
}

#[test]
fn run_writes_dream_notes_under_dreams_dir() {
    let l = Ledger::open_in_memory().expect("ledger");
    l.apply_facet_v2_schema().expect("schema");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "wi",
            title: "wi",
            created_at: Utc::now(),
        })
        .expect("workitem");
    for i in 0..3 {
        let turns = vec![
            InteractionTurn {
                ai_says: format!("ai {i}"),
                ai_turn_uuid: format!("ai-{i}"),
                user_says: "user".to_string(),
                user_turn_uuid: format!("u-{i}"),
                tags: vec![],
            },
            InteractionTurn {
                ai_says: format!("ai2 {i}"),
                ai_turn_uuid: format!("ai2-{i}"),
                user_says: "ack".to_string(),
                user_turn_uuid: format!("u2-{i}"),
                tags: vec![],
            },
        ];
        l.upsert_gem(NewGem {
            workitem_id: wid,
            session_uuid: "s1",
            task: "repeated task",
            context_loaded: &[],
            context_missing: &[],
            interaction: &turns,
            review: &Review::default(),
            tags: &[],
            why_it_matters: "m",
            extractor_model: "sonnet",
            extracted_at: ts(2026, 5, 1 + i, 12),
        })
        .expect("gem");
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = Config::default();
    let report = run(&cfg, &l, tmp.path()).expect("run");
    // Both semantic-duplicate (repeated task) and narrative-candidate
    // (3 gems in s1, no narrative) should surface.
    assert!(report.dreams_discovered >= 1);
    assert_eq!(report.notes_written, report.dreams_discovered);
    // Files exist under the configured dreams_dir.
    let dreams_dir = tmp.path().join(&cfg.vault.dreams_dir);
    let entries: Vec<_> = std::fs::read_dir(&dreams_dir)
        .expect("readdir")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty());
}
