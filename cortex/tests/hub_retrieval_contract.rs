//! The hub retrieval contract, asserted under the REAL BGE tokenizer
//! (`docs/design/2026-08-15-entity-hub-two-vector-synthesis.md`, Phase 2).
//!
//! For a both-vector hub, the text cortex actually embeds
//! (`title + capture_note + summary`) must carry at least one SESSION claim AND
//! at least one SOURCE claim inside the encoder's 512-token window
//! (`vault/src/embedding/candle.rs` `MAX_SEQ_LEN`, which truncates silently).
//! The `## Summary` byte budget is a render-path proxy for that; the assertion
//! is the tokenizer.
//!
//! Two things make this test worth having:
//!
//! - It runs on a MEGA-HUB fixture sized from the real `entities/claude.md`
//!   cohort (345 claim-bearing source members, 63 session members). A
//!   small-fixture string assertion proves nothing about truncation.
//! - It asserts BOTH vectors survive. Asserting one is the half-test that let
//!   digest starvation flip which vector was invisible between panel rounds.
//!
//! It runs offline in `otto ci` from a committed `tokenizer.json` fixture
//! (~711 KB): tokenization needs no model weights, so nothing downloads.

use std::path::PathBuf;

use cortex::config::RenderConfig;
use cortex::embed::summary_embed_text;
use cortex::hub::{HubMember, render_hub_body};
use tokenizers::{Tokenizer, TruncationParams};
use vault::distilled::Claim;

/// The encoder's hard cap. `vault::embedding::candle::MAX_SEQ_LEN` is private,
/// so it is restated here with the assertion that pins it (a model-card limit
/// declared by bge-small-en-v1.5; a change there is a deliberate model change).
const MAX_SEQ_LEN: usize = 512;

/// The real `claude.md` cohort under the simulated post-Phase-1 membership.
const SOURCE_MEMBERS: usize = 345;
const SESSION_MEMBERS: usize = 63;

fn tokenizer() -> Tokenizer {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bge-small-en-v1.5-tokenizer.json");
    let mut tokenizer = Tokenizer::from_file(&fixture).expect("load the committed BGE tokenizer fixture");
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_SEQ_LEN,
            ..Default::default()
        }))
        .expect("truncation params (mirrors vault::embedding::candle::build_inner)");
    tokenizer
}

fn member(path: &str, title: &str, note_type: &str, date: &str, claims: &[String]) -> HubMember {
    HubMember {
        path: path.to_string(),
        title: title.to_string(),
        note_type: note_type.to_string(),
        date: Some(date.to_string()),
        claims: claims
            .iter()
            .map(|t| Claim {
                text: t.clone(),
                ..Claim::default()
            })
            .collect(),
    }
}

/// Claim text shaped like real distilled claims: sentence-length, no
/// punctuation, so a wordpiece round-trip through `decode` is comparable.
fn source_claim(i: usize, j: usize) -> String {
    format!(
        "Source note {i:03} claim {j} observes that retrieval quality depends on how the corpus is chunked and embedded"
    )
}

fn session_claim(i: usize, j: usize) -> String {
    format!("Session note {i:03} claim {j} records that the agent rewrote the pipeline and the tests went green")
}

/// The `claude.md`-sized fixture: 345 source members and 63 session members,
/// three claims each (hundreds of source claims, tens of session claims).
fn mega_hub_members() -> Vec<HubMember> {
    let mut members = Vec::with_capacity(SOURCE_MEMBERS + SESSION_MEMBERS);
    for i in 0..SOURCE_MEMBERS {
        let claims: Vec<String> = (0..3).map(|j| source_claim(i, j)).collect();
        members.push(member(
            &format!("knowledge/tech/source-{i:03}.md"),
            &format!("Source {i:03}"),
            if i % 2 == 0 { "article" } else { "youtube" },
            // Descending dates so the newest members render first.
            &format!("2026-{:02}-{:02}", (i % 12) + 1, (i % 28) + 1),
            &claims,
        ));
    }
    for i in 0..SESSION_MEMBERS {
        let claims: Vec<String> = (0..3).map(|j| session_claim(i, j)).collect();
        members.push(member(
            &format!("sessions/session-{i:03}.md"),
            &format!("Session {i:03}"),
            "session",
            &format!("2026-08-{:02}", (i % 15) + 1),
            &claims,
        ));
    }
    members
}

/// The exact text cortex embeds for a hub: the indexer fills `notes.summary`
/// from `parse_body_summary`, and `cortex::embed` composes
/// `title + capture_note + summary` (a hub has no capture note).
fn embed_text(title: &str, body: &str) -> String {
    let summary = vault::search::parse_body_summary(body).expect("a rendered hub body carries a ## Summary section");
    summary_embed_text(title, "", &summary)
}

/// The first claim the body renders under `heading` - i.e. the first claim of
/// that vector in body order, which is exactly the first claim its digest line
/// carries (the digest draws from the same capped member set, never a second
/// selection rule). Taking the needle FROM the body is what keeps this an
/// assertion about the contract rather than about fixture ordering.
fn first_claim_under(body: &str, heading: &str) -> String {
    let section = body.split(heading).nth(1).expect("section present");
    let bullet = section
        .lines()
        .find(|l| l.starts_with("- "))
        .expect("section has a claim bullet");
    let text = bullet.trim_start_matches("- ");
    let link_at = text.rfind(" ([[").expect("every bullet is wikilinked to its member");
    text[..link_at].to_string()
}

/// Decode the tokens that SURVIVE truncation, so "inside the window" is asserted
/// against what the encoder would actually see, not against the input string.
fn window_text(tokenizer: &Tokenizer, text: &str) -> (usize, String) {
    let encoding = tokenizer.encode(text, true).expect("encode");
    let ids = encoding.get_ids().to_vec();
    let decoded = tokenizer.decode(&ids, true).expect("decode");
    (ids.len(), decoded)
}

#[test]
fn a_mega_hub_carries_both_vectors_inside_the_512_token_window() {
    let members = mega_hub_members();
    let caps = RenderConfig::default();
    let body = render_hub_body("claude", &members, &caps).expect("mega hub renders");

    // Sanity on the fixture itself: both vectors are present at cohort scale.
    assert!(body.contains("## From sources"), "sources section present");
    assert!(body.contains("## From your sessions"), "sessions section present");

    let summary = vault::search::parse_body_summary(&body).expect("## Summary section");
    assert!(
        summary.starts_with("claude: hub of 345 sources and 63 sessions."),
        "the definition sentence states FULL claim-bearing membership: {summary}"
    );
    let emitted = summary.len() + 1; // the digest plus its trailing newline
    assert!(
        emitted <= caps.summary_byte_budget,
        "the digest fits its byte budget: {emitted} bytes"
    );

    let text = embed_text("claude", &body);
    let tok = tokenizer();
    let (tokens, window) = window_text(&tok, &text);
    assert!(
        tokens <= MAX_SEQ_LEN,
        "the whole embed text fits the window ({tokens} tokens)"
    );

    // THE contract: both vectors reach the embedding.
    let session_needle = first_claim_under(&body, "## From your sessions").to_lowercase();
    let source_needle = first_claim_under(&body, "## From sources").to_lowercase();
    assert!(
        window.contains(&session_needle),
        "a SESSION claim must land inside the 512-token window.\nneedle: {session_needle}\nwindow: {window}"
    );
    assert!(
        window.contains(&source_needle),
        "a SOURCE claim must land inside the 512-token window.\nneedle: {source_needle}\nwindow: {window}"
    );
}

#[test]
fn an_unbudgeted_digest_would_starve_the_second_vector() {
    // The control that gives the assertion above its teeth: remove the byte
    // budget and the digest runs thousands of tokens, so the encoder's silent
    // truncation eats the trailing vector entirely. This is the live failure
    // mode the budget exists to prevent - not a hypothetical.
    let members = mega_hub_members();
    let caps = RenderConfig {
        summary_byte_budget: 1_000_000,
        ..RenderConfig::default()
    };
    let body = render_hub_body("claude", &members, &caps).expect("renders");
    let text = embed_text("claude", &body);
    let tok = tokenizer();
    let (tokens, window) = window_text(&tok, &text);
    assert!(
        tokens >= MAX_SEQ_LEN,
        "an unbudgeted digest overruns the window ({tokens} tokens)"
    );
    let source_needle = first_claim_under(&body, "## From sources").to_lowercase();
    assert!(
        !window.contains(&source_needle),
        "without the budget the trailing SOURCE vector is truncated away"
    );
}

#[test]
fn a_small_both_vector_hub_still_carries_both_vectors() {
    // entities/terraform.md live: 8 session / 4 external members.
    let mut members = Vec::new();
    for i in 0..4 {
        members.push(member(
            &format!("knowledge/tech/tf-{i}.md"),
            &format!("TF {i}"),
            "article",
            "2026-03-01",
            &[source_claim(i, 0)],
        ));
    }
    for i in 0..8 {
        members.push(member(
            &format!("sessions/tf-{i}.md"),
            &format!("TF session {i}"),
            "session",
            "2026-08-01",
            &[session_claim(i, 0)],
        ));
    }
    let body = render_hub_body("terraform", &members, &RenderConfig::default()).expect("renders");
    let text = embed_text("terraform", &body);
    let tok = tokenizer();
    let (tokens, window) = window_text(&tok, &text);
    assert!(tokens <= MAX_SEQ_LEN, "{tokens} tokens");
    assert!(
        window.contains(&first_claim_under(&body, "## From your sessions").to_lowercase()),
        "session claim in window: {window}"
    );
    assert!(
        window.contains(&first_claim_under(&body, "## From sources").to_lowercase()),
        "source claim in window: {window}"
    );
}
