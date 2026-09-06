use super::*;

#[test]
fn test_extract_search_terms() {
    let content = "Rust programming language for building CLI tools with great performance";
    let terms = extract_search_terms(content, 10);
    assert!(!terms.is_empty());
    // Content words should be included
    assert!(terms.contains(&"rust".to_string()));
    assert!(terms.contains(&"programming".to_string()));
    assert!(terms.contains(&"building".to_string()));
    // Stop words should be excluded
    assert!(!terms.contains(&"for".to_string()));
    assert!(!terms.contains(&"with".to_string()));
}

#[test]
fn test_extract_search_terms_empty_input() {
    let terms = extract_search_terms("", 5);
    assert!(terms.is_empty());
}

#[test]
fn test_extract_search_terms_respects_limit() {
    let content = "one two three four five six seven eight nine ten eleven twelve";
    let terms = extract_search_terms(content, 3);
    assert!(terms.len() <= 3);
}

#[test]
fn test_open_memory_index() {
    let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
    let stats = index.stats().expect("Failed to get stats");
    assert_eq!(stats.total_notes, 0);
}

#[test]
fn test_domain_stats_empty() {
    let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
    let stats = index.domain_stats().expect("Failed to get domain stats");
    assert!(stats.is_empty());
}

#[test]
fn test_tag_domain_map_empty() {
    let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
    let map = index.tag_domain_map().expect("Failed to get tag domain map");
    assert!(map.is_empty());
}

#[test]
fn test_find_similar_empty_content() {
    let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
    let results = index.find_similar("", 5).expect("Failed find_similar");
    assert!(results.is_empty());
}

#[test]
fn test_tag_search_exact() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "Rust CLI", "tech", &["rust", "cli"], "body");
    insert_test_note(&index, "notes/b.md", "Rust Web", "tech", &["rust", "web"], "body");
    insert_test_note(&index, "notes/c.md", "Python ML", "ai", &["python", "ml"], "body");

    let results = index.tag_search("rust", None, None).expect("tag_search");
    assert_eq!(results.len(), 2);

    let results = index.tag_search("python", None, None).expect("tag_search");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "notes/c.md");
}

#[test]
fn test_tag_search_prefix() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "Rust CLI", "tech", &["rust", "rust-cli"], "body");
    insert_test_note(&index, "notes/b.md", "Ruby", "tech", &["ruby"], "body");

    let results = index.tag_search("rust*", None, None).expect("tag_search prefix");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "notes/a.md");
}

#[test]
fn test_tag_search_with_domain_filter() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "AI Rust", "ai", &["rust"], "body");
    insert_test_note(&index, "notes/b.md", "Tech Rust", "tech", &["rust"], "body");

    let results = index.tag_search("rust", Some("ai"), None).expect("tag_search domain");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "notes/a.md");
}

#[test]
fn test_tag_stats() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "A", "tech", &["rust", "cli"], "body");
    insert_test_note(&index, "notes/b.md", "B", "tech", &["rust", "web"], "body");
    insert_test_note(&index, "notes/c.md", "C", "ai", &["rust", "ml"], "body");

    let stats = index.tag_stats().expect("tag_stats");
    let rust_stat = stats.iter().find(|s| s.tag == "rust").expect("rust tag");
    assert_eq!(rust_stat.count, 3);
    assert!(rust_stat.domains.contains(&"tech".to_string()));
    assert!(rust_stat.domains.contains(&"ai".to_string()));

    let cli_stat = stats.iter().find(|s| s.tag == "cli").expect("cli tag");
    assert_eq!(cli_stat.count, 1);
}

#[test]
fn test_tag_cooccurrence() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "A", "tech", &["rust", "cli", "linux"], "body");
    insert_test_note(&index, "notes/b.md", "B", "tech", &["rust", "web"], "body");
    insert_test_note(&index, "notes/c.md", "C", "ai", &["python", "ml"], "body");

    let cooccur = index.tag_cooccurrence("rust").expect("cooccurrence");
    // cli, linux, web all co-occur with rust
    assert_eq!(cooccur.len(), 3);
    assert!(cooccur.iter().any(|(t, c)| t == "cli" && *c == 1));
    assert!(cooccur.iter().any(|(t, c)| t == "web" && *c == 1));
    assert!(cooccur.iter().any(|(t, c)| t == "linux" && *c == 1));
}

#[test]
fn test_extract_wikilinks_simple() {
    let body = "See [[some-note]] and [[another-note]] for details.";
    let links = extract_wikilinks(body);
    assert_eq!(links, vec!["some-note", "another-note"]);
}

#[test]
fn test_extract_wikilinks_with_alias() {
    let body = "Check [[some-note|display text]] here.";
    let links = extract_wikilinks(body);
    assert_eq!(links, vec!["some-note"]);
}

#[test]
fn test_extract_wikilinks_with_heading() {
    let body = "See [[some-note#heading]] for the section.";
    let links = extract_wikilinks(body);
    assert_eq!(links, vec!["some-note"]);
}

#[test]
fn test_extract_wikilinks_skips_code_blocks() {
    let body = "Before\n```\n[[code-link]]\n```\nAfter [[real-link]]";
    let links = extract_wikilinks(body);
    assert_eq!(links, vec!["real-link"]);
}

#[test]
fn test_extract_host() {
    assert_eq!(
        extract_host("https://www.youtube.com/watch?v=abc"),
        Some("youtube.com".to_string())
    );
    assert_eq!(
        extract_host("https://github.com/user/repo"),
        Some("github.com".to_string())
    );
    assert_eq!(extract_host("http://example.com"), Some("example.com".to_string()));
    assert_eq!(extract_host("not-a-url"), None);
}

#[test]
fn test_creator_stats() {
    let index = SearchIndex::open_memory().expect("open");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('a.md', 'A', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', '', 'Alice', '', '', 0)",
            [],
        ).expect("insert");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('b.md', 'B', 'ai', 'youtube', 'assisted', '', '2026-03-21', '[]', '', 'Alice', '', '', 0)",
            [],
        ).expect("insert");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('c.md', 'C', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', 'Bob', '', '', 0)",
            [],
        ).expect("insert");

    let stats = index.creator_stats().expect("creator_stats");
    assert_eq!(stats[0], ("Alice".to_string(), 2));
    assert_eq!(stats[1], ("Bob".to_string(), 1));
}

#[test]
fn test_source_domain_stats() {
    let index = SearchIndex::open_memory().expect("open");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('a.md', 'A', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', 'https://www.youtube.com/watch?v=abc', '', '', '', 0)",
            [],
        ).expect("insert");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('b.md', 'B', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', 'https://youtube.com/watch?v=def', '', '', '', 0)",
            [],
        ).expect("insert");
    index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('c.md', 'C', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', 'https://github.com/user/repo', '', '', '', 0)",
            [],
        ).expect("insert");

    let stats = index.source_domain_stats().expect("source_domain_stats");
    assert_eq!(stats[0], ("youtube.com".to_string(), 2));
    assert_eq!(stats[1], ("github.com".to_string(), 1));
}

#[test]
fn test_find_outbound_links() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "A", "tech", &[], "See [[b]] and [[c|see C]].");
    insert_test_note(&index, "notes/b.md", "B", "tech", &[], "Just body.");

    let links = index.find_outbound_links("notes/a.md").expect("outbound");
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].target, "b");
    assert_eq!(links[1].target, "c");
}

#[test]
fn test_find_inbound_links() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "notes/a.md", "A", "tech", &[], "Links to [[b]].");
    insert_test_note(&index, "notes/b.md", "B", "tech", &[], "No links.");
    insert_test_note(&index, "notes/c.md", "C", "tech", &[], "Also links to [[b]].");

    let inbound = index.find_inbound_links("notes/b.md").expect("inbound");
    assert_eq!(inbound.len(), 2);
    let paths: Vec<&str> = inbound.iter().map(|n| n.path.as_str()).collect();
    assert!(paths.contains(&"notes/a.md"));
    assert!(paths.contains(&"notes/c.md"));
}

#[test]
fn test_governance_columns_exist() {
    let index = SearchIndex::open_memory().expect("open");
    // Insert a note with governance fields via direct SQL
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality, classified, classified_by, confidence, needs_review, duplicate_group)
                 VALUES ('test.md', 'Test', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', '', '', 0, 'high', 1, 'deterministic', 'high', 0, '')",
                [],
            )
            .expect("insert with governance columns");

    let quality: String = index
        .conn
        .query_row("SELECT quality FROM notes WHERE path = 'test.md'", [], |row| row.get(0))
        .expect("query quality");
    assert_eq!(quality, "high");

    let classified: i64 = index
        .conn
        .query_row("SELECT classified FROM notes WHERE path = 'test.md'", [], |row| {
            row.get(0)
        })
        .expect("query classified");
    assert_eq!(classified, 1);
}

#[test]
fn test_inbox_notes() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(&index, "inbox/a.md", "Inbox A", "tech", &[], "body");
    insert_test_note(&index, "inbox/b.md", "Inbox B", "", &[], "body");
    insert_test_note(&index, "notes/c.md", "Not inbox", "tech", &[], "body");

    let inbox = index.inbox_notes(None).expect("inbox");
    assert_eq!(inbox.len(), 2);
}

#[test]
fn test_inbox_oldest() {
    let index = SearchIndex::open_memory().expect("open");
    // dotfile: excluded even though it is the oldest by modified_at
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('inbox/.claude/loop.md', 'Loop', '', 'article', 'assisted', '', '2026-01-01', '[]', '', '', '', '', 100)",
            [],
        )
        .expect("insert dotfile note");
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('inbox/newer.md', 'Newer', '', 'article', 'assisted', '', '2026-03-21', '[]', '', '', '', '', 300)",
            [],
        )
        .expect("insert newer note");
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('inbox/oldest.md', 'Oldest', '', 'article', 'assisted', '', '2026-01-15', '[]', '', '', '', '', 200)",
            [],
        )
        .expect("insert oldest note");

    let oldest = index.inbox_oldest().expect("inbox_oldest").expect("some row");
    assert_eq!(oldest.0, "inbox/oldest.md");
    assert_eq!(oldest.1, 200);
}

#[test]
fn test_inbox_oldest_empty() {
    let index = SearchIndex::open_memory().expect("open");
    let oldest = index.inbox_oldest().expect("inbox_oldest");
    assert!(oldest.is_none());
}

#[test]
fn test_quality_distribution() {
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('a.md', 'A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'high')",
                [],
            )
            .expect("insert");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('b.md', 'B', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'low')",
                [],
            )
            .expect("insert");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('c.md', 'C', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'high')",
                [],
            )
            .expect("insert");

    let dist = index.quality_distribution().expect("distribution");
    assert_eq!(dist.len(), 2);
    assert!(dist.iter().any(|(q, c)| q == "high" && *c == 2));
    assert!(dist.iter().any(|(q, c)| q == "low" && *c == 1));
}

#[test]
fn test_note_quality() {
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('a.md', 'A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'low')",
                [],
            )
            .expect("insert");

    assert_eq!(index.note_quality("a.md").expect("query"), Some("low".to_string()));
    assert_eq!(
        index.note_quality("missing.md").expect("query"),
        None,
        "absent note => None"
    );
}

#[test]
fn test_classify_stats() {
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
                 VALUES ('notes/a.md', 'A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 1, 'deterministic', 'high', 0)",
                [],
            )
            .expect("insert");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
                 VALUES ('inbox/b.md', 'B', '', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 0, '', '', 1)",
                [],
            )
            .expect("insert");

    let stats = index.classify_stats(None).expect("classify_stats");
    assert_eq!(stats.total_classified, 1);
    assert_eq!(stats.pending_review, 1);
    assert_eq!(stats.inbox_count, 1);
    assert_eq!(stats.unclassified, 1);

    // Domain filter is parameterized: a value with SQL-special characters must
    // be treated as data (matching nothing here), never interpolated as SQL.
    let filtered = index
        .classify_stats(Some("tech' OR '1'='1"))
        .expect("classify_stats with injection-shaped domain must not error");
    assert_eq!(filtered.total_classified, 0);

    // And a legitimate domain filter still narrows correctly.
    let tech = index.classify_stats(Some("tech")).expect("classify_stats tech");
    assert_eq!(tech.total_classified, 1);
}

#[test]
fn test_duplicate_groups() {
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('a.md', 'Article A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-1')",
                [],
            )
            .expect("insert");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('b.md', 'Article A Copy', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-1')",
                [],
            )
            .expect("insert");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('c.md', 'Solo', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-solo')",
                [],
            )
            .expect("insert");

    let groups = index.duplicate_groups().expect("duplicate_groups");
    // Only group-1 has more than 1 note
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].group_id, "group-1");
    assert_eq!(groups[0].note_count, 2);
}

#[test]
fn parse_body_summary_extracts_section() {
    let body = "# Title\n\n## Summary\n\nA two-sentence summary. With a follow-up.\n\n## Claims\n- one\n";
    let summary = parse_body_summary(body).expect("summary present");
    assert_eq!(summary, "A two-sentence summary. With a follow-up.");
}

#[test]
fn parse_body_summary_returns_none_when_section_missing() {
    let body = "# Title\n\n## Notes\n\nNo summary here.\n";
    assert!(parse_body_summary(body).is_none());
}

#[test]
fn parse_body_summary_returns_none_when_section_empty() {
    let body = "# Title\n\n## Summary\n\n## Claims\n";
    assert!(parse_body_summary(body).is_none());
}

#[test]
fn parse_body_summary_handles_trailing_section() {
    let body = "# Title\n\n## Summary\n\nLast section in the document.\n";
    let summary = parse_body_summary(body).expect("summary present");
    assert_eq!(summary, "Last section in the document.");
}

#[test]
fn parse_body_claims_extracts_bullets_and_anchors() {
    let body = "## Summary\n\nx\n\n## Claims\n- First claim. [12:34]\n- Second claim with no anchor\n- Third claim. [section-three]\n\n## Links\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 3);
    assert_eq!(claims[0].text, "First claim.");
    assert_eq!(claims[0].anchor.as_deref(), Some("12:34"));
    assert_eq!(claims[1].text, "Second claim with no anchor");
    assert!(claims[1].anchor.is_none());
    assert_eq!(claims[2].text, "Third claim.");
    assert_eq!(claims[2].anchor.as_deref(), Some("section-three"));
}

#[test]
fn parse_body_claims_returns_empty_when_section_missing() {
    let body = "# Title\n\nNo claims section.\n";
    assert!(parse_body_claims(body).is_empty());
}

#[test]
fn parse_body_claims_ignores_similar_headings() {
    // A user heading like "## My Notes" must NOT be parsed as claims.
    let body = "## My Notes\n- looks like a claim but isn't\n\n## Claims\n- real claim\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "real claim");
}

#[test]
fn parse_body_claims_skips_blank_bullets() {
    let body = "## Claims\n- \n- A real claim\n-\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "A real claim");
}

#[test]
fn parse_body_claims_does_not_extract_anchor_when_brackets_are_inline() {
    // Brackets in the middle of the text stay in the text. Only a [...]
    // group at the very end of the line is an anchor.
    let body = "## Claims\n- See [docs] for context.\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "See [docs] for context.");
    assert!(claims[0].anchor.is_none());
}

#[test]
fn parse_body_claims_strips_kind_who_and_quote_decoration() {
    // Phase 3: FTS text must be the clean claim sentence, with the
    // `**kind**` / `(who)` prefix, trailing `[anchor]`, and `  > "quote"`
    // continuation line all peeled off — but recovered into the fields.
    let body = concat!(
        "## Claims\n",
        "- **position** (@simonw): Orchestration beats autonomy. [00:14:30]\n",
        "  > \"the harness does the thinking\"\n",
        "\n## Links\n",
    );
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "Orchestration beats autonomy.");
    assert_eq!(claims[0].anchor.as_deref(), Some("00:14:30"));
    assert_eq!(claims[0].kind, crate::distilled::ClaimKind::Position);
    assert_eq!(claims[0].who.as_deref(), Some("@simonw"));
    assert_eq!(claims[0].quote.as_deref(), Some("the harness does the thinking"));
}

#[test]
fn parse_body_claims_strips_kind_only_prefix() {
    let body = "## Claims\n- **recommendation**: Pin the model version.\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "Pin the model version.");
    assert!(claims[0].who.is_none());
}

#[test]
fn parse_body_claims_leaves_unknown_bold_prefix_in_text() {
    // A legacy claim that legitimately opens with bold text (not a claim kind)
    // must NOT be stripped — the whole sentence stays as the FTS text.
    let body = "## Claims\n- **Important** takeaway about caching.\n";
    let claims = parse_body_claims(body);
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].text, "**Important** takeaway about caching.");
    assert_eq!(claims[0].kind, crate::distilled::ClaimKind::Fact);
}

#[test]
fn index_one_insert_zeroes_signal_columns() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("inbox/new.md", "# T\n\n## Summary\n\nHello.\n");
    let action = index.index_one(&note, 100).expect("index_one");
    assert_eq!(action, IndexAction::Inserted);

    let (hits, last, inbound) = signal_row(&index, "inbox/new.md");
    assert_eq!(hits, 0);
    assert!(last.is_none());
    assert_eq!(inbound, 0);
}

#[test]
fn index_one_update_preserves_signal_columns() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("inbox/keep.md", "# T\n\n## Summary\n\nFirst pass.\n");
    index.index_one(&note, 100).expect("first index");

    // Pretend Doc 3 (or anyone) wrote signal values out-of-band.
    index
        .conn
        .execute(
            "UPDATE notes SET search_hit_count = ?1, last_accessed_at = ?2,
                                  inbound_link_count = ?3
                 WHERE path = ?4",
            params![17_i64, 999_999_i64, 3_i64, "inbox/keep.md"],
        )
        .expect("seed signals");

    // Reindex with new content + new mtime.
    let updated = make_test_note("inbox/keep.md", "# T\n\n## Summary\n\nRevised body.\n");
    let action = index.index_one(&updated, 200).expect("reindex");
    assert_eq!(action, IndexAction::Updated);

    let (hits, last, inbound) = signal_row(&index, "inbox/keep.md");
    assert_eq!(hits, 17, "search_hit_count must survive reindex");
    assert_eq!(last, Some(999_999), "last_accessed_at must survive reindex");
    assert_eq!(inbound, 3, "inbound_link_count must survive reindex");

    // And the vault-derived columns DID get updated.
    let summary: String = index
        .conn
        .query_row("SELECT summary FROM notes WHERE path = 'inbox/keep.md'", [], |row| {
            row.get(0)
        })
        .expect("summary");
    assert_eq!(summary, "Revised body.");
}

// --- entity-hub-two-vector-synthesis Phase 2 -------------------------------

/// The structural half of the hub-body membership contract: only DELIBERATE
/// note->hub kinds count, and a hub is never a member of a hub. Both filters
/// live in SQL, not in a caller's convention, so no consumer can forget them.
#[test]
fn hub_members_deliberate_keeps_only_deliberate_note_to_hub_edges() {
    let mut index = SearchIndex::open_memory().expect("open");
    let hub = "entities/claude.md";
    for path in [
        hub,
        "entities/agents.md",
        "notes/wikilinked.md",
        "notes/repo-member.md",
        "notes/creator-member.md",
        "notes/source-member.md",
        "notes/semantic.md",
        "notes/shared-tag.md",
    ] {
        index
            .insert_test_note_graph(path, &[], "", "", "tech", "b", 100)
            .expect("note");
    }
    let edges = vec![
        Edge::deterministic("notes/wikilinked.md", hub, "wikilink", 1.0),
        Edge::deterministic("notes/repo-member.md", hub, "repo-member", 1.0),
        Edge::deterministic("notes/creator-member.md", hub, "creator-member", 1.0),
        Edge::deterministic("notes/source-member.md", hub, "source-member", 1.0),
        Edge::deterministic("notes/semantic.md", hub, "semantic", 0.9),
        Edge::deterministic("notes/shared-tag.md", hub, "shared-tag", 0.5),
        // A hub linking a hub: deliberate KIND, but it would feed hub bodies
        // (refusals included) back into other hub bodies.
        Edge::deterministic("entities/agents.md", hub, "wikilink", 1.0),
        Edge::fact("entities/agents.md", hub, "relates-to", 1.0, "notes/x.md"),
    ];
    index.insert_edges(&edges).expect("insert");

    assert_eq!(
        index.hub_members_deliberate(hub).expect("members"),
        vec![
            "notes/creator-member.md".to_string(),
            "notes/repo-member.md".to_string(),
            "notes/source-member.md".to_string(),
            "notes/wikilinked.md".to_string(),
        ],
        "deliberate note->hub kinds only, sorted by src, no entities/% src"
    );
    // The kind-agnostic probe still sees everything (graph tests use it as a
    // generic inbound counter); only the builder's view is narrowed.
    assert_eq!(
        index.hub_members(hub).expect("members").len(),
        7,
        "hub_members stays kind-agnostic"
    );
}

/// A hub with only inferred inbound (semantic / shared-tag) has ZERO builder
/// membership - it keeps its stub rather than rendering claims from notes the
/// author never associated with the subject.
#[test]
fn hub_members_deliberate_is_empty_for_inferred_only_membership() {
    let mut index = SearchIndex::open_memory().expect("open");
    let hub = "entities/every.md";
    for path in [hub, "notes/a.md", "notes/b.md"] {
        index
            .insert_test_note_graph(path, &[], "", "", "tech", "b", 100)
            .expect("note");
    }
    index
        .insert_edges(&[
            Edge::deterministic("notes/a.md", hub, "semantic", 0.9),
            Edge::deterministic("notes/b.md", hub, "shared-tag", 0.4),
        ])
        .expect("insert");
    assert!(
        index.hub_members_deliberate(hub).expect("members").is_empty(),
        "inferred edges are not membership"
    );
}

// ---- FTS5 term quoting: the "no such column" fail-open ----

#[test]
fn fts_quote_wraps_and_escapes() {
    assert_eq!(fts_quote("plain"), "\"plain\"");
    assert_eq!(fts_quote("xda-developers"), "\"xda-developers\"");
    assert_eq!(fts_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
}

#[test]
fn find_similar_survives_hyphenated_and_uuid_terms() {
    // The exact shapes that aborted the MATCH in production: a hyphenated host
    // (`no such column: developers`), a note slug (`no such column: plugin`),
    // and a UUID (`no such column: 6dc0`).
    let index = SearchIndex::open_memory().expect("open");

    let body = "captured from xda-developers about cli-plugin-marketplace-sync-incident-and-fix-options \
                during session dfb3bc2f-6dc0-4151-bf48-4789bad13782 on tatari-tv infrastructure";
    insert_test_note(
        &index,
        "notes/hyphenated.md",
        "Hyphenated Title",
        "tech",
        &["tech"],
        body,
    );

    let hits = index
        .find_similar(body, 5)
        .expect("hyphenated terms must not abort the MATCH");
    assert!(
        hits.iter().any(|r| r.path == "notes/hyphenated.md"),
        "expected the source note back, got {:?}",
        hits.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

#[test]
fn find_similar_survives_operator_keywords_and_colons() {
    let index = SearchIndex::open_memory().expect("open");

    // `and`/`or`/`near` are FTS5 operators; `07:51` and `scott:idler` are the
    // colon shape that reads as a column filter.
    let body = "meeting at 07:51 near the office about scott:idler and marquee or clyde tooling";
    insert_test_note(&index, "notes/operators.md", "Operators", "tech", &["tech"], body);

    let hits = index
        .find_similar(body, 5)
        .expect("operator words must not abort the MATCH");
    assert!(hits.iter().any(|r| r.path == "notes/operators.md"));
}

#[test]
fn search_propagates_a_malformed_match_instead_of_returning_empty() {
    let index = SearchIndex::open_memory().expect("open");
    insert_test_note(
        &index,
        "notes/present.md",
        "Present",
        "tech",
        &["tech"],
        "some body text",
    );

    // A raw unquoted hyphenated bareword: sqlite rejects it mid-step. The old
    // code swallowed that per row and returned Ok(vec![]) - a fail-open search.
    let err = index
        .search("xda-developers", None, None, None, Some(5))
        .expect_err("a malformed MATCH must be an error, not an empty result set");
    assert!(
        format!("{err:#}").contains("fts5 search failed"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn find_similar_lossy_degrades_to_empty_without_panicking() {
    let index = SearchIndex::open_memory().expect("open");
    // Nothing indexed: no hits, no error, no panic.
    assert!(index.find_similar_lossy("anything at all", 5).is_empty());
}
