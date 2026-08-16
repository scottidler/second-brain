use super::*;

/// A member whose claims are REAL claim text, not a placeholder. The previous
/// mechanism shipped broken precisely because its tests injected a double that
/// ignored its members argument; every test here goes through the real renderer
/// with real claims.
fn member(path: &str, title: &str, note_type: &str, date: Option<&str>, claims: &[&str]) -> HubMember {
    HubMember {
        path: path.to_string(),
        title: title.to_string(),
        note_type: note_type.to_string(),
        date: date.map(|d| d.to_string()),
        claims: claims
            .iter()
            .map(|t| Claim {
                text: (*t).to_string(),
                ..Claim::default()
            })
            .collect(),
    }
}

fn caps() -> RenderConfig {
    RenderConfig::default()
}

/// The digest as `parse_body_summary` would recover it: the `## Summary`
/// section's text.
fn digest_of(body: &str) -> String {
    vault::search::parse_body_summary(body).expect("rendered body carries a ## Summary section")
}

#[test]
fn both_vectors_render_both_sections_with_claim_text_and_wikilinks() {
    let members = vec![
        member(
            "knowledge/tech/context-rot.md",
            "Context Rot",
            "article",
            Some("2026-05-01"),
            &["Long contexts degrade recall past 60k tokens"],
        ),
        member(
            "sessions/2026-08-01-oracle.md",
            "oracle retrieval work",
            "session",
            Some("2026-08-01"),
            &["Vector-only retrieval beat equal-weight hybrid on this corpus"],
        ),
    ];
    let body = render_hub_body("claude", &members, &caps()).expect("renders");

    assert!(body.contains("## From sources"), "{body}");
    assert!(body.contains("## From your sessions"), "{body}");
    // The claim TEXT lands in the body, wikilinked to the member it came from -
    // not the member's bare path, and not a paraphrase.
    assert!(
        body.contains("- Long contexts degrade recall past 60k tokens ([[knowledge/tech/context-rot|Context Rot]])"),
        "{body}"
    );
    assert!(
        body.contains(
            "- Vector-only retrieval beat equal-weight hybrid on this corpus ([[sessions/2026-08-01-oracle|oracle retrieval work]])"
        ),
        "{body}"
    );
    // Sources section comes first in the body; sessions lead in the digest.
    let sources_at = body.find("## From sources").expect("sources heading");
    let sessions_at = body.find("## From your sessions").expect("sessions heading");
    assert!(sources_at < sessions_at, "sources section precedes sessions: {body}");
    let digest = digest_of(&body);
    assert!(
        digest.find("Sessions:").expect("sessions line") < digest.find("Sources:").expect("sources line"),
        "sessions lead the digest (scarcer vector, and the tail is what truncation eats): {digest}"
    );
}

#[test]
fn one_vector_hub_names_only_the_vector_it_has() {
    let members = vec![member(
        "knowledge/tech/a.md",
        "A",
        "youtube",
        Some("2026-01-01"),
        &["A source claim"],
    )];
    let body = render_hub_body("rag", &members, &caps()).expect("renders");
    let digest = digest_of(&body);
    assert!(
        digest.starts_with("rag: hub of 1 source."),
        "never \"and 0 sessions\": {digest}"
    );
    assert!(
        !digest.contains("Sessions:"),
        "absent vector has no digest line: {digest}"
    );
    assert!(
        !body.contains("## From your sessions"),
        "absent vector has no body section: {body}"
    );
}

#[test]
fn definition_sentence_counts_full_claim_bearing_membership_not_the_capped_set() {
    // 25 claim-bearing sources against a cap of 20: the sentence five oracle
    // tldr handlers render must state 25, not the capped 20.
    let members: Vec<HubMember> = (0..25)
        .map(|i| {
            member(
                &format!("knowledge/tech/s{i:02}.md"),
                &format!("S{i:02}"),
                "article",
                Some("2026-01-01"),
                &["claim text"],
            )
        })
        .collect();
    let body = render_hub_body("claude", &members, &caps()).expect("renders");
    assert!(
        digest_of(&body).starts_with("claude: hub of 25 sources."),
        "{}",
        digest_of(&body)
    );
    assert_eq!(
        body.matches("## From sources").count(),
        1,
        "exactly one sources heading"
    );
    assert!(
        body.contains("...and 5 more claim-bearing members"),
        "deterministic overflow line: {body}"
    );
    assert_eq!(
        body.lines().filter(|l| l.starts_with("- ")).count(),
        20,
        "the section is capped at max-members-per-section"
    );
}

#[test]
fn members_sort_by_date_descending_then_path_with_undated_last() {
    let members = vec![
        member("z/old.md", "Old", "article", Some("2024-01-01"), &["old claim"]),
        member("a/undated.md", "Undated", "article", None, &["undated claim"]),
        member("b/new.md", "New B", "article", Some("2026-08-01"), &["new b claim"]),
        member("a/new.md", "New A", "article", Some("2026-08-01"), &["new a claim"]),
    ];
    let body = render_hub_body("t", &members, &caps()).expect("renders");
    let order: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("- "))
        .map(|l| l.trim_start_matches("- ").split(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(
        order,
        vec!["new", "new", "old", "undated"],
        "date desc, path tiebreak, undated last: {body}"
    );
    // The path tiebreak inside the 2026-08-01 group: a/new before b/new.
    let a_at = body.find("a/new").expect("a/new");
    let b_at = body.find("b/new").expect("b/new");
    assert!(a_at < b_at, "path-ascending tiebreak within one date: {body}");
}

#[test]
fn claims_are_capped_per_member_and_stay_in_note_order() {
    let cfg = RenderConfig {
        max_claims_per_member: 2,
        ..RenderConfig::default()
    };
    let members = vec![member(
        "k/a.md",
        "A",
        "article",
        Some("2026-01-01"),
        &["first", "second", "third"],
    )];
    let body = render_hub_body("t", &members, &cfg).expect("renders");
    assert!(body.contains("- first ("), "{body}");
    assert!(body.contains("- second ("), "{body}");
    assert!(!body.contains("- third ("), "capped at 2 claims per member: {body}");
    assert!(
        body.find("- first").unwrap() < body.find("- second").unwrap(),
        "claims stay in note order"
    );
}

#[test]
fn other_typed_members_are_excluded_and_render_nothing() {
    // entities/usa-football.md live: its only claim-bearing deliberate member is
    // an `image` note - neither source nor session, so the hub keeps its stub.
    let members = vec![member(
        "k/photo.md",
        "Photo",
        "image",
        Some("2026-01-01"),
        &["an image claim"],
    )];
    assert!(
        render_hub_body("usa-football", &members, &caps()).is_none(),
        "an other-typed member is not a vector"
    );
}

#[test]
fn no_claim_bearing_member_renders_none() {
    let members = vec![
        member("k/a.md", "A", "article", Some("2026-01-01"), &[]),
        member("s/b.md", "B", "session", Some("2026-01-01"), &[]),
    ];
    assert!(render_hub_body("t", &members, &caps()).is_none());
    assert!(render_hub_body("t", &[], &caps()).is_none(), "memberless hub");
}

#[test]
fn rendered_body_parses_to_zero_claims() {
    // Belt and suspenders on the hub-feeds-hub hole: the SQL predicate excludes
    // `entities/%` sources, and the renderer emits no `## Claims` heading, so a
    // hub body is inert to the claim parser even if an edge ever slipped through.
    let members = vec![
        member("k/a.md", "A", "article", Some("2026-01-01"), &["a source claim"]),
        member("s/b.md", "B", "session", Some("2026-01-01"), &["a session claim"]),
    ];
    let body = render_hub_body("claude", &members, &caps()).expect("renders");
    assert!(
        vault::search::parse_body_claims(&body).is_empty(),
        "a rendered hub body carries zero parseable claims: {body}"
    );
}

#[test]
fn render_is_byte_identical_across_runs() {
    let members = vec![
        member("k/a.md", "A", "article", Some("2026-01-01"), &["one", "two"]),
        member("s/b.md", "B", "session", Some("2026-02-02"), &["three"]),
        member("k/c.md", "C", "youtube", None, &["four"]),
    ];
    let a = render_hub_body("claude", &members, &caps()).expect("renders");
    let b = render_hub_body("claude", &members, &caps()).expect("renders");
    assert_eq!(a, b, "pure function of membership + claims");
    // Member order in the input must not change the output either (the index
    // returns rows sorted by src; the renderer re-sorts).
    let mut shuffled = members.clone();
    shuffled.reverse();
    assert_eq!(
        render_hub_body("claude", &shuffled, &caps()).expect("renders"),
        a,
        "input order does not leak into the body"
    );
}

// --- the digest byte math, pinned -----------------------------------------

#[test]
fn both_vectors_split_the_remaining_budget_with_the_odd_byte_to_sources() {
    // Everything below is UTF-8 BYTES of the exact emitted text. The definition
    // sentence and its newline are counted FIRST; the remainder splits with
    // integer division, sessions first, and the ODD byte goes to sources.
    let definition = "t: hub of 1 source and 1 session.";
    let remaining = 41; // odd -> sessions 20, sources 21
    let cfg = RenderConfig {
        summary_byte_budget: definition.len() + 1 + remaining,
        ..RenderConfig::default()
    };
    // "Sessions: " (10) + 9 + "\n" (1) == 20 exactly.
    // "Sources: "  (9)  + 11 + "\n" (1) == 21 exactly - it fits ONLY because the
    // odd byte went to sources.
    let members = vec![
        member("s/a.md", "A", "session", Some("2026-01-01"), &["sessclaim"]),
        member("k/b.md", "B", "article", Some("2026-01-01"), &["src-claim-x"]),
    ];
    let body = render_hub_body("t", &members, &cfg).expect("renders");
    assert_eq!(
        digest_of(&body),
        format!("{definition}\nSessions: sessclaim\nSources: src-claim-x"),
        "byte-exact digest"
    );
    // parse_body_summary trims; the emitted section text carries the newline.
    assert!(body.contains(&format!("{definition}\nSessions: sessclaim\nSources: src-claim-x\n")));
}

#[test]
fn a_vector_line_costs_its_label_joiners_and_newline() {
    // Two session claims: "Sessions: " (10) + "aaa" (3) + "; " (2) + "bbb" (3)
    // + "\n" (1) = 19. A budget one byte short drops the second claim whole.
    let definition = "t: hub of 1 session.";
    let fits = RenderConfig {
        summary_byte_budget: definition.len() + 1 + 19,
        ..RenderConfig::default()
    };
    let short = RenderConfig {
        summary_byte_budget: definition.len() + 1 + 18,
        ..RenderConfig::default()
    };
    let members = vec![member("s/a.md", "A", "session", Some("2026-01-01"), &["aaa", "bbb"])];
    assert_eq!(
        digest_of(&render_hub_body("t", &members, &fits).expect("renders")),
        format!("{definition}\nSessions: aaa; bbb"),
    );
    assert_eq!(
        digest_of(&render_hub_body("t", &members, &short).expect("renders")),
        format!("{definition}\nSessions: aaa"),
        "the joiner and the newline are part of the line's cost"
    );
}

#[test]
fn unused_session_budget_cedes_to_sources_one_directionally() {
    let definition = "t: hub of 1 source and 1 session.";
    let remaining = 40; // sessions 20, sources 20
    let cfg = RenderConfig {
        summary_byte_budget: definition.len() + 1 + remaining,
        ..RenderConfig::default()
    };
    // "Sessions: x\n" costs 12, so 8 bytes cede: sources gets 20 + 8 = 28, and
    // "Sources: " (9) + 18 + "\n" (1) = 28 fits whole. At a bare 20 it would
    // have been truncated.
    let long_source = "abcdefghijklmnopqr"; // 18 bytes
    let members = vec![
        member("s/a.md", "A", "session", Some("2026-01-01"), &["x"]),
        member("k/b.md", "B", "article", Some("2026-01-01"), &[long_source]),
    ];
    let digest = digest_of(&render_hub_body("t", &members, &cfg).expect("renders"));
    assert_eq!(digest, format!("{definition}\nSessions: x\nSources: {long_source}"));
    assert!(!digest.contains("..."), "no truncation once the slack ceded: {digest}");
}

#[test]
fn sessions_never_exceed_their_own_budget() {
    // The floor for sources is only enforceable because sessions are capped:
    // a session vector with more claims than fit stops at its half, leaving the
    // source line intact.
    let definition = "t: hub of 1 source and 1 session.";
    let remaining = 40; // sessions 20, sources 20
    let cfg = RenderConfig {
        summary_byte_budget: definition.len() + 1 + remaining,
        max_claims_per_member: 10,
        ..RenderConfig::default()
    };
    let members = vec![
        member(
            "s/a.md",
            "A",
            "session",
            Some("2026-01-01"),
            &["aaaa", "bbbb", "cccc", "dddd", "eeee"],
        ),
        member("k/b.md", "B", "article", Some("2026-01-01"), &["src"]),
    ];
    let digest = digest_of(&render_hub_body("t", &members, &cfg).expect("renders"));
    let sessions_line = digest
        .lines()
        .find(|l| l.starts_with("Sessions:"))
        .expect("sessions line");
    let emitted = sessions_line.len() + 1; // the line plus its trailing newline
    assert!(emitted <= 20, "sessions stay inside their half: {sessions_line:?}");
    assert!(
        digest.contains("Sources: src"),
        "the source line survives a greedy session vector: {digest}"
    );
}

#[test]
fn an_overflowing_first_claim_is_truncated_on_a_utf8_boundary_with_room_for_the_ellipsis() {
    // Dropping it would zero a real vector (the longest live member claims blob
    // is 3460 bytes), so the FIRST claim is truncated instead - and the cut
    // lands on a character boundary, never mid code point.
    let definition = "t: hub of 1 session.";
    let cfg = RenderConfig {
        summary_byte_budget: definition.len() + 1 + 20,
        ..RenderConfig::default()
    };
    // "Sessions: " (10) + head + "..." (3) + "\n" (1) <= 20 -> head <= 6 bytes.
    // Byte 6 lands INSIDE the two-byte `é` (bytes 5..7), so the cut backs off to 5.
    let claim = "abcdeéfgh";
    let members = vec![member("s/a.md", "A", "session", Some("2026-01-01"), &[claim])];
    let body = render_hub_body("t", &members, &cfg).expect("renders");
    let digest = digest_of(&body);
    assert_eq!(digest, format!("{definition}\nSessions: abcde..."));
    let emitted = digest.len() + 1; // the digest plus its trailing newline
    assert!(
        emitted <= cfg.summary_byte_budget,
        "the truncated line INCLUDING the ellipsis fits the budget"
    );
}

#[test]
fn the_digest_never_exceeds_the_byte_budget() {
    // The budget binds on every shape: one vector, both vectors, huge claims,
    // and a budget too small for any claim line at all.
    let long: String = "x".repeat(4_000);
    let unicode = "héllo wörld ".repeat(50);
    let definition = "claude: hub of 2 sources and 1 session.";
    let members = vec![
        member(
            "s/a.md",
            "A",
            "session",
            Some("2026-01-01"),
            &[long.as_str(), unicode.as_str()],
        ),
        member(
            "k/b.md",
            "B",
            "article",
            Some("2026-01-01"),
            &[unicode.as_str(), long.as_str()],
        ),
        member("k/c.md", "C", "youtube", Some("2025-01-01"), &["short"]),
    ];
    for budget in [1usize, 40, 61, 120, 401, 1_200, 4_096] {
        let cfg = RenderConfig {
            summary_byte_budget: budget,
            ..RenderConfig::default()
        };
        let body = render_hub_body("claude", &members, &cfg).expect("renders");
        let section = vault::search::parse_body_summary(&body).expect("summary section");
        // The definition sentence is always emitted, budget or not - it is what
        // `first_sentence` returns for the five tldr handlers - so a budget
        // smaller than the sentence yields the sentence alone.
        assert!(section.starts_with(definition), "budget {budget}: {section}");
        if budget < definition.len() + 1 {
            assert_eq!(section, definition, "budget {budget} leaves room for no claim line");
        } else {
            // The emitted digest is the section plus its trailing newline.
            let emitted = section.len() + 1;
            assert!(emitted <= budget, "budget {budget}: digest is {emitted} bytes");
        }
    }
}

#[test]
fn digest_claims_come_from_the_same_capped_member_set_as_the_body() {
    // Never a second selection rule: a claim in the digest is a claim the body
    // renders, in body order.
    let cfg = RenderConfig {
        max_members_per_section: 1,
        max_claims_per_member: 1,
        ..RenderConfig::default()
    };
    let members = vec![
        member("k/newer.md", "Newer", "article", Some("2026-08-01"), &["newer claim"]),
        member("k/older.md", "Older", "article", Some("2020-01-01"), &["older claim"]),
    ];
    let body = render_hub_body("t", &members, &cfg).expect("renders");
    let digest = digest_of(&body);
    assert!(digest.contains("Sources: newer claim"), "{digest}");
    assert!(
        !digest.contains("older claim"),
        "a capped-out member contributes nothing to the digest: {digest}"
    );
}

#[test]
fn vector_classifies_the_two_ingestion_vectors_and_nothing_else() {
    for t in ["youtube", "article", "github", "social", "research"] {
        assert_eq!(Vector::of(t), Vector::Source, "{t}");
    }
    assert_eq!(Vector::of("session"), Vector::Session);
    for t in ["image", "pdf", "note", "entity", "", "reddit"] {
        assert_eq!(Vector::of(t), Vector::Other, "{t}");
    }
}

#[test]
fn a_member_title_carrying_wikilink_syntax_drops_the_alias() {
    let mut m = member("k/a.md", "Weird [title] | here", "article", Some("2026-01-01"), &["c"]);
    let body = render_hub_body("t", std::slice::from_ref(&m), &caps()).expect("renders");
    assert!(body.contains("([[k/a]])"), "no broken alias markup: {body}");
    m.title = String::new();
    let body = render_hub_body("t", &[m], &caps()).expect("renders");
    assert!(
        body.contains("([[k/a]])"),
        "empty title falls back to the target: {body}"
    );
}
