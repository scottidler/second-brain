use chrono::Utc;

use super::*;
use crate::ledger::clusters::NewClusterAssignment;
use crate::ledger::moments::NewJudgmentMoment;
use crate::ledger::sessions::UpsertSession;
use crate::ledger::workitems::{NewWorkItem, SessionContribution};

fn open() -> Ledger {
    Ledger::open_in_memory().expect("open in-memory")
}

#[test]
fn fresh_ledger_at_current_schema() {
    let l = open();
    assert_eq!(l.schema_version().expect("schema version"), schema::CURRENT_VERSION);
}

#[test]
fn migrate_is_idempotent() {
    let l = open();
    schema::migrate(&l).expect("re-migrate");
    schema::migrate(&l).expect("re-migrate2");
    assert_eq!(l.schema_version().expect("schema version"), schema::CURRENT_VERSION);
}

#[test]
fn upsert_session_inserts_then_updates() {
    let l = open();
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/home/me/repos/x",
        repo_slug: Some("me/x"),
        seen_at: now,
    })
    .expect("first upsert");
    let row = l.get_session("s1").expect("get").expect("present");
    assert_eq!(row.cwd, "/home/me/repos/x");
    assert_eq!(row.repo_slug.as_deref(), Some("me/x"));
    assert_eq!(row.last_cluster_offset, 0);

    let later = now + chrono::Duration::seconds(60);
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/home/me/repos/x",
        repo_slug: Some("me/x"),
        seen_at: later,
    })
    .expect("second upsert");
    let row = l.get_session("s1").expect("get").expect("present");
    assert!(row.last_seen_at > row.first_seen_at);
}

#[test]
fn cluster_offset_advances() {
    let l = open();
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/cwd",
        repo_slug: None,
        seen_at: now,
    })
    .expect("upsert");
    l.set_cluster_offset("s1", 4096, Some("turn-uuid-7"))
        .expect("set offset");
    let row = l.get_session("s1").expect("get").expect("present");
    assert_eq!(row.last_cluster_offset, 4096);
    assert_eq!(row.last_cluster_turn_uuid.as_deref(), Some("turn-uuid-7"));
}

#[test]
fn workitem_insert_query_with_repos_modes() {
    let l = open();
    let now = Utc::now();
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "loopr-v5",
            title: "Loopr v5",
            created_at: now,
        })
        .expect("insert");
    l.link_workitem_repo(wid, "scottidler/loopr").expect("link");
    l.link_workitem_repo(wid, "scottidler/loopr")
        .expect("link2 (idempotent)");

    let w = l.workitem_by_slug("loopr-v5").expect("query").expect("present");
    assert_eq!(w.id, wid);
    assert_eq!(w.title, "Loopr v5");
    assert_eq!(w.repos, vec!["scottidler/loopr"]);
    assert_eq!(w.sessions_count, 0);
    assert!(w.modes_present.is_empty());
}

#[test]
fn duplicate_slug_errors() {
    let l = open();
    let now = Utc::now();
    l.insert_workitem(NewWorkItem {
        slug: "alpha",
        title: "Alpha",
        created_at: now,
    })
    .expect("first");
    let err = l.insert_workitem(NewWorkItem {
        slug: "alpha",
        title: "Different",
        created_at: now,
    });
    assert!(err.is_err(), "expected unique violation");
}

#[test]
fn record_contribution_then_count() {
    let l = open();
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/c",
        repo_slug: None,
        seen_at: now,
    })
    .expect("session");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "x",
            title: "X",
            created_at: now,
        })
        .expect("wi");
    l.record_contribution(SessionContribution {
        session_uuid: "s1",
        workitem_id: wid,
        at: now,
    })
    .expect("contrib");
    let w = l.workitem_by_id(wid).expect("query").expect("present");
    assert_eq!(w.sessions_count, 1);
}

#[test]
fn cluster_assignments_idempotent_pending_then_marked() {
    let l = open();
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/c",
        repo_slug: None,
        seen_at: now,
    })
    .expect("s");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "x",
            title: "X",
            created_at: now,
        })
        .expect("wi");
    let cid = l
        .insert_cluster_assignment(NewClusterAssignment {
            session_uuid: "s1",
            workitem_id: wid,
            first_turn_uuid: "u1",
            last_turn_uuid: "u5",
            clustered_at: now,
            cluster_model: "haiku",
        })
        .expect("ca");
    let again = l
        .insert_cluster_assignment(NewClusterAssignment {
            session_uuid: "s1",
            workitem_id: wid,
            first_turn_uuid: "u1",
            last_turn_uuid: "u5",
            clustered_at: now,
            cluster_model: "haiku",
        })
        .expect("ca again");
    assert_eq!(cid, again, "duplicate insert returns same id");

    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].extracted);

    l.mark_extracted(cid).expect("mark");
    let pending = l.pending_cluster_assignments(10).expect("pending2");
    assert!(pending.is_empty());
}

#[test]
fn moments_idempotent_on_unique() {
    let l = open();
    let now = Utc::now();
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "x",
            title: "X",
            created_at: now,
        })
        .expect("wi");
    let m = NewJudgmentMoment {
        workitem_id: wid,
        session_uuid: "s1",
        turn_uuid: "tu1",
        mode: "reject",
        ai_move: "proposed bad name",
        scott_move: "rejected and renamed",
        quote_excerpt: "no, call it foo",
        why_it_matters: "naming sets taste",
        extractor_model: "sonnet",
        extracted_at: now,
    };
    l.insert_moment(m.clone()).expect("first");
    l.insert_moment(m).expect("second (idempotent)");
    let v = l.moments_for_workitem(wid).expect("query");
    assert_eq!(v.len(), 1);
}

#[test]
fn meta_set_and_get() {
    let l = open();
    assert!(l.meta_get("k").expect("meta get").is_none());
    l.meta_set("k", "v").expect("set");
    assert_eq!(l.meta_get("k").expect("get").as_deref(), Some("v"));
    l.meta_set("k", "v2").expect("update");
    assert_eq!(l.meta_get("k").expect("get").as_deref(), Some("v2"));
}

#[test]
fn mark_dormant_flips_inactive() {
    let l = open();
    let long_ago = Utc::now() - chrono::Duration::days(30);
    let recent = Utc::now() - chrono::Duration::days(2);
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "s1",
        cwd: "/c",
        repo_slug: None,
        seen_at: long_ago,
    })
    .expect("s1");
    l.upsert_session(UpsertSession {
        session_uuid: "s2",
        cwd: "/c",
        repo_slug: None,
        seen_at: recent,
    })
    .expect("s2");

    let old_wid = l
        .insert_workitem(NewWorkItem {
            slug: "old",
            title: "Old",
            created_at: long_ago,
        })
        .expect("old wi");
    l.record_contribution(SessionContribution {
        session_uuid: "s1",
        workitem_id: old_wid,
        at: long_ago,
    })
    .expect("old contrib");

    let new_wid = l
        .insert_workitem(NewWorkItem {
            slug: "new",
            title: "New",
            created_at: recent,
        })
        .expect("new wi");
    l.record_contribution(SessionContribution {
        session_uuid: "s2",
        workitem_id: new_wid,
        at: recent,
    })
    .expect("new contrib");

    let n = l.mark_dormant(now, 14).expect("mark");
    assert_eq!(n, 1, "only the 30-day-old work-item should flip dormant");
    let old = l.workitem_by_slug("old").expect("q").expect("p");
    let new = l.workitem_by_slug("new").expect("q").expect("p");
    assert!(matches!(old.status, crate::workitem::WorkItemStatus::Dormant));
    assert!(matches!(new.status, crate::workitem::WorkItemStatus::Active));
}
