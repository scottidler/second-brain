use chrono::Utc;

use super::*;
use crate::config::Config;
use crate::fabric::FakeFabric;
use crate::ledger::Ledger;
use crate::ledger::moments::NewJudgmentMoment;
use crate::ledger::workitems::NewWorkItem;

fn seed(l: &Ledger, moments: usize) -> Vec<i64> {
    let now = Utc::now();
    let mut ids = Vec::new();
    for i in 0..moments {
        let slug = format!("wi-{i}");
        let wid = l
            .insert_workitem(NewWorkItem {
                slug: &slug,
                title: &format!("Work item {i}"),
                created_at: now,
            })
            .expect("insert");
        ids.push(wid);
        l.insert_moment(NewJudgmentMoment {
            workitem_id: wid,
            session_uuid: &format!("sess-{i}"),
            turn_uuid: &format!("t-{i}"),
            mode: "reject",
            ai_move: "proposed plausibly wrong thing",
            scott_move: "rejected and renamed",
            quote_excerpt: &format!("no, that's not right, do it like X{i}"),
            why_it_matters: "naming sets taste",
            extractor_model: "sonnet",
            extracted_at: now,
        })
        .expect("moment");
    }
    ids
}

#[tokio::test]
async fn skips_when_too_few_moments() {
    let l = Ledger::open_in_memory().expect("ledger");
    seed(&l, 1);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    // The LLM should never be called.
    let out = portrait_for_mode("reject", &cfg, &l, &fabric).await.expect("call");
    assert!(out.is_none());
}

#[tokio::test]
async fn synthesises_a_portrait_note() {
    let l = Ledger::open_in_memory().expect("ledger");
    seed(&l, 5);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-portrait",
        "title: \"How Scott rejects plausible-but-wrong\"\nbody: |\n  Paragraph one names a shape.\n\n  Paragraph two names another.\nmoments_cited:\n  - workitem_slug: wi-0\n    short_description: rejected a premature abstraction\n  - workitem_slug: wi-1\n    short_description: rejected a bad name\n",
    );
    let body = portrait_for_mode("reject", &cfg, &l, &fabric)
        .await
        .expect("call")
        .expect("got a portrait body");
    assert!(body.contains("# How Scott rejects plausible-but-wrong"));
    assert!(body.contains("Paragraph one names a shape."));
    assert!(body.contains("## Representative moments"));
    assert!(body.contains("[[work-items/wi-0]]"));
    assert!(body.contains("type: facet-portrait"));
    assert!(body.contains("facet-mode: reject"));
}

#[tokio::test]
async fn llm_can_request_skip_with_empty_title() {
    let l = Ledger::open_in_memory().expect("ledger");
    seed(&l, 3);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-portrait", "title: \"\"\nbody: \"\"\nmoments_cited: []\n");
    let out = portrait_for_mode("reject", &cfg, &l, &fabric).await.expect("call");
    assert!(out.is_none(), "empty title from LLM means skip");
}
