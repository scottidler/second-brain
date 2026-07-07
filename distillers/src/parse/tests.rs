use super::*;
use vault::distilled::{Claim, ClaimKind};

#[test]
fn strips_yaml_fence() {
    let raw = "```yaml\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_bare_fence() {
    let raw = "```\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn passes_through_unfenced() {
    let raw = "summary: hi\nclaims: []";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims: []");
}

#[test]
fn unfenced_with_embedded_fence_is_not_truncated() {
    // Regression for the truncation bug: unfenced YAML whose content contains
    // an embedded code fence must NOT be cut at that fence.
    let raw = "summary: see the snippet\nclaims:\n  - text: \"```rust let x = 1; ```\"";
    let out = strip_fences(raw);
    assert!(
        out.contains("let x = 1;"),
        "embedded fence content was truncated: {out:?}"
    );
}

#[test]
fn fenced_with_trailing_prose_strips_to_close() {
    let raw = "```yaml\nsummary: hi\n```\nignored trailing";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_surrounding_whitespace() {
    // Leading/trailing whitespace around the fence (LLMs add blank lines) is
    // trimmed before the fence is detected.
    let raw = "\n\n  ```yaml\nsummary: hi\n```  \n\n";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_multiline_yaml_body() {
    // A multi-line body inside the fence is returned intact (only the fence
    // markers are removed), so every consumer's serde_yaml parse sees clean
    // YAML.
    let raw = "```yaml\nsummary: hi\nclaims:\n  - text: one\n  - text: two\n```";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims:\n  - text: one\n  - text: two");
}

#[test]
fn preserves_colons_inside_unfenced_body() {
    // Colons inside values must survive untouched (no fence present).
    let raw = "summary: \"ratio is 3:1 at 12:00\"";
    assert_eq!(strip_fences(raw), "summary: \"ratio is 3:1 at 12:00\"");
}

#[test]
fn approx_tokens_uses_four_char_rule() {
    assert_eq!(approx_tokens(0), 0);
    assert_eq!(approx_tokens(4), 1);
    assert_eq!(approx_tokens(401), 100);
}

fn claim(text: &str, anchor: Option<&str>) -> Claim {
    Claim {
        text: text.to_string(),
        anchor: anchor.map(|s| s.to_string()),
        ..Default::default()
    }
}

fn pattern_claim(text: &str, anchor: Option<&str>) -> PatternClaim {
    PatternClaim {
        text: text.to_string(),
        anchor: anchor.map(|s| s.to_string()),
        kind: ClaimKind::default(),
        who: None,
        quote: None,
    }
}

#[test]
fn build_reduce_input_has_two_labeled_sections_with_anchor_prefixed_pool() {
    let summaries = vec!["First chunk summary.".to_string(), "Second chunk summary.".to_string()];
    let pool = vec![
        claim("An anchored claim.", Some("00:00:05")),
        claim("A claim without an anchor.", None),
    ];
    let input = build_reduce_input(&summaries, &pool, &[], None);

    assert!(input.contains("## Chunk Summaries"));
    assert!(input.contains("## Claim Pool"));
    assert!(input.contains("First chunk summary.\n\nSecond chunk summary."));
    assert!(
        input.contains("[00:00:05] An anchored claim."),
        "anchored pool line: {input:?}"
    );
    assert!(
        input.contains("A claim without an anchor."),
        "anchorless pool line: {input:?}"
    );
    // The summaries section precedes the claim pool section.
    let summaries_at = input.find("## Chunk Summaries").expect("summaries heading present");
    let pool_at = input.find("## Claim Pool").expect("pool heading present");
    assert!(summaries_at < pool_at);
}

#[test]
fn build_reduce_input_normalizes_bracketed_pool_anchor() {
    // A pool claim whose anchor already carries brackets is not double-bracketed.
    let pool = vec![claim("Bracketed anchor claim.", Some("[00:01:00]"))];
    let input = build_reduce_input(&[], &pool, &[], None);
    assert!(input.contains("[00:01:00] Bracketed anchor claim."), "{input:?}");
    assert!(!input.contains("[[00:01:00]]"));
}

#[test]
fn select_reduce_claims_keeps_pool_matching_anchor() {
    let pool = vec![claim("Pooled.", Some("00:25:00"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Selected late claim.", Some("00:25:00"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].anchor.as_deref(), Some("00:25:00"));
    assert_eq!(stripped, 0);
}

#[test]
fn select_reduce_claims_matches_across_bracket_normalization() {
    // Pool anchor bare, selected anchor bracketed — still a match.
    let pool = vec![claim("Pooled.", Some("00:25:00"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Selected.", Some("[00:25:00]"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(
        selected[0].anchor.as_deref(),
        Some("00:25:00"),
        "normalized to bracket-free form"
    );
    assert_eq!(stripped, 0);
}

#[test]
fn select_reduce_claims_strips_non_pool_anchor_and_counts() {
    let pool = vec![claim("Pooled.", Some("00:00:05"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Invented-anchor claim.", Some("09:09:09"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1, "claim text retained");
    assert!(selected[0].anchor.is_none(), "invented anchor stripped");
    assert_eq!(selected[0].text, "Invented-anchor claim.");
    assert_eq!(stripped, 1);
}

#[test]
fn select_reduce_claims_accepts_anchorless_synthesis() {
    // No anchor → accepted as a synthesis, no text-match gate against the pool.
    let pool = vec![
        claim("Pooled one.", Some("00:00:05")),
        claim("Pooled two.", Some("00:10:00")),
    ];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("A brand-new synthesis spanning two pool claims.", None)],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1);
    assert!(selected[0].anchor.is_none());
    assert_eq!(
        stripped, 0,
        "an anchorless synthesis is not counted as a stripped anchor"
    );
}

#[test]
fn select_reduce_claims_empty_returns_none() {
    let pool = vec![claim("Pooled.", Some("00:00:05"))];
    let mut stripped = 0;
    assert!(select_reduce_claims(vec![], &pool, &mut stripped).is_none());
    // A claim with only whitespace text is skipped, yielding an empty selection.
    assert!(select_reduce_claims(vec![pattern_claim("   ", None)], &pool, &mut stripped).is_none());
    assert_eq!(stripped, 0);
}

#[test]
fn select_reduce_claims_accepts_anchorless_pool_as_synthesis() {
    // Articles/threads carry no anchors: the pool is anchorless and every
    // selected claim (also anchorless) is accepted as a synthesis, no invention
    // gate tripped, nothing stripped.
    let pool = vec![
        claim("An anchorless article claim.", None),
        claim("Another anchorless article claim.", None),
    ];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![
            pattern_claim("A selected article claim.", None),
            pattern_claim("A synthesized article claim.", None),
        ],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|c| c.anchor.is_none()));
    assert_eq!(stripped, 0, "no anchors to strip in an anchorless pool");
}

#[test]
fn build_thread_reduce_input_prepends_verbatim_thread_head() {
    let head = "@simonw: Original post where the thread metadata lives.";
    let summaries = vec!["First chunk summary.".to_string()];
    let pool = vec![claim("An anchorless thread claim.", None)];
    let input = build_thread_reduce_input(head, &summaries, &pool);

    assert!(input.contains("## Thread Head"), "{input:?}");
    assert!(input.contains(head), "the head is carried verbatim: {input:?}");
    assert!(input.contains("## Chunk Summaries"));
    assert!(input.contains("## Claim Pool"));
    // Head precedes the summaries (author/post-count context comes first).
    let head_at = input.find("## Thread Head").expect("head heading");
    let summaries_at = input.find("## Chunk Summaries").expect("summaries heading");
    assert!(head_at < summaries_at);
}

fn enum_candidate(name: &str, text: &str, anchor: Option<&str>, ordinal: Option<u32>) -> EnumCandidate {
    EnumCandidate {
        name: name.to_string(),
        text: text.to_string(),
        anchor: anchor.map(|s| s.to_string()),
        ordinal,
    }
}

#[test]
fn build_reduce_input_omits_enumeration_section_when_no_candidates() {
    // The whole section is absent when no chunk found a candidate — this is the
    // reduce pattern's gate signal for `enumeration: null`.
    let input = build_reduce_input(&["S.".to_string()], &[], &[], None);
    assert!(!input.contains("## Enumeration Candidates"), "{input:?}");
    assert!(!input.contains("Declared count"), "{input:?}");
}

#[test]
fn build_reduce_input_renders_enumeration_candidates_section() {
    let candidates = vec![
        enum_candidate("Codex Plugin", "A plugin.", Some("00:01:00"), Some(1)),
        enum_candidate("Aider", "A CLI.", Some("[00:02:30]"), None),
    ];
    let input = build_reduce_input(&["S.".to_string()], &[], &candidates, Some(10));
    assert!(input.contains("## Enumeration Candidates"), "{input:?}");
    assert!(input.contains("Declared count: 10"), "{input:?}");
    // Anchor-prefixed, ordinal `#1`, ` - ` separator (no em dash).
    assert!(input.contains("[00:01:00] #1 Codex Plugin - A plugin."), "{input:?}");
    // Missing ordinal renders `#?`; bracketed candidate anchor normalized.
    assert!(input.contains("[00:02:30] #? Aider - A CLI."), "{input:?}");
    // The section follows the claim pool.
    let pool_at = input.find("## Claim Pool").expect("pool heading");
    let enum_at = input.find("## Enumeration Candidates").expect("enum heading");
    assert!(pool_at < enum_at);
}

#[test]
fn build_reduce_input_candidate_without_anchor_has_no_bracket_prefix() {
    let candidates = vec![enum_candidate("Item", "desc", None, None)];
    let input = build_reduce_input(&[], &[], &candidates, None);
    // No `Declared count` line when None; no leading `[` bracket on the line.
    assert!(!input.contains("Declared count"), "{input:?}");
    assert!(input.contains("#? Item - desc"), "{input:?}");
    assert!(!input.contains("[#?"), "{input:?}");
}

#[test]
fn resolve_reduce_enumeration_keeps_candidate_matching_anchor() {
    let candidates = vec![enum_candidate("A", "a", Some("00:01:00"), Some(1))];
    let parsed = PatternEnumeration {
        lead_in: Some("Two tools:".to_string()),
        declared_count: Some(2),
        items: vec![
            PatternEnumeratedItem {
                name: "A".to_string(),
                text: "a".to_string(),
                anchor: Some("00:01:00".to_string()),
            },
            PatternEnumeratedItem {
                name: "B".to_string(),
                text: "b".to_string(),
                anchor: None,
            },
        ],
    };
    let mut stripped = 0;
    let enumeration = resolve_reduce_enumeration(parsed, &candidates, &mut stripped).expect("enumeration");
    assert_eq!(enumeration.items.len(), 2);
    assert_eq!(enumeration.items[0].anchor.as_deref(), Some("00:01:00"));
    assert!(enumeration.items[1].anchor.is_none());
    assert_eq!(stripped, 0);
    assert_eq!(enumeration.declared_count, Some(2));
}

#[test]
fn resolve_reduce_enumeration_strips_anchor_absent_from_candidates() {
    // The Phase 0 concern: a description-lifted timestamp not present in any
    // chunk candidate is dishonest — stripped and counted, item text retained.
    let candidates = vec![enum_candidate("A", "a", Some("00:01:00"), Some(1))];
    let parsed = PatternEnumeration {
        lead_in: None,
        declared_count: None,
        items: vec![PatternEnumeratedItem {
            name: "A".to_string(),
            text: "a".to_string(),
            anchor: Some("05:30".to_string()), // from the description, not a transcript position
        }],
    };
    let mut stripped = 0;
    let enumeration = resolve_reduce_enumeration(parsed, &candidates, &mut stripped).expect("enumeration");
    assert_eq!(enumeration.items.len(), 1, "item text retained");
    assert!(enumeration.items[0].anchor.is_none(), "dishonest anchor stripped");
    assert_eq!(stripped, 1);
}

#[test]
fn resolve_reduce_enumeration_strips_all_anchors_when_pool_anchorless() {
    // Articles: the candidate pool carries no anchors, so any item anchor the
    // model produced is not a real position and is stripped.
    let candidates = vec![enum_candidate("A", "a", None, Some(1))];
    let parsed = PatternEnumeration {
        lead_in: None,
        declared_count: None,
        items: vec![PatternEnumeratedItem {
            name: "A".to_string(),
            text: "a".to_string(),
            anchor: Some("00:01:00".to_string()),
        }],
    };
    let mut stripped = 0;
    let enumeration = resolve_reduce_enumeration(parsed, &candidates, &mut stripped).expect("enumeration");
    assert!(enumeration.items[0].anchor.is_none());
    assert_eq!(stripped, 1);
}

#[test]
fn resolve_reduce_enumeration_empty_items_returns_none() {
    let parsed = PatternEnumeration {
        lead_in: Some("nothing".to_string()),
        declared_count: Some(3),
        items: vec![],
    };
    let mut stripped = 0;
    assert!(resolve_reduce_enumeration(parsed, &[], &mut stripped).is_none());
}

#[test]
fn pattern_enumeration_into_enumeration_filters_empty_named_items() {
    let parsed = PatternEnumeration {
        lead_in: Some("  ".to_string()), // whitespace lead-in dropped
        declared_count: Some(1),
        items: vec![
            PatternEnumeratedItem {
                name: "  ".to_string(), // empty name dropped
                text: "orphan".to_string(),
                anchor: None,
            },
            PatternEnumeratedItem {
                name: "Real".to_string(),
                text: "kept".to_string(),
                anchor: None,
            },
        ],
    };
    let enumeration = parsed.into_enumeration().expect("one real item survives");
    assert_eq!(enumeration.items.len(), 1);
    assert_eq!(enumeration.items[0].name, "Real");
    assert!(enumeration.lead_in.is_none(), "whitespace lead-in dropped");
}

#[test]
fn pattern_yaml_without_new_phase4_keys_still_parses() {
    // Fallback safety: pre-Phase-4 pattern output (no tldr/enumeration/
    // key-ideas/declared-count/enumeration-candidates keys) must still
    // deserialize, with the new fields defaulting to None.
    let raw = "summary: \"A summary.\"\nclaims:\n  - text: \"A claim.\"\ntags: [rust]\nlinks: []\n";
    let parsed: PatternYaml = serde_yaml::from_str(raw).expect("legacy pattern output parses");
    assert_eq!(parsed.summary.as_deref(), Some("A summary."));
    assert!(parsed.tldr.is_none());
    assert!(parsed.enumeration.is_none());
    assert!(parsed.key_ideas.is_none());
    assert!(parsed.declared_count.is_none());
    assert!(parsed.enumeration_candidates.is_none());
}

#[test]
fn reduce_yaml_without_new_phase4_keys_still_parses() {
    // Fallback safety for the reduce leaf: a pre-Phase-4 reduce output
    // (summary + claims only) parses with the new fields defaulted.
    let raw = "summary: \"Reduced.\"\nclaims:\n  - text: \"A claim.\"\n    anchor: \"00:00:05\"\n";
    let parsed: ReduceYaml = serde_yaml::from_str(raw).expect("legacy reduce output parses");
    assert_eq!(parsed.summary.as_deref(), Some("Reduced."));
    assert!(parsed.tldr.is_none());
    assert!(parsed.enumeration.is_none());
    assert!(parsed.key_ideas.is_none());
    assert!(parsed.claims.is_some());
}

#[test]
fn pattern_yaml_with_enumeration_parses() {
    // The single-call enumeration block round-trips into the typed leaf.
    let raw = "summary: \"S.\"\ntldr: \"The hook.\"\nenumeration:\n  lead_in: \"Two tools:\"\n  declared_count: 2\n  items:\n    - name: \"A\"\n      text: \"first\"\n      anchor: \"00:01:00\"\n    - name: \"B\"\n      text: \"second\"\n      anchor: \"00:02:00\"\nkey_ideas:\n  - \"**Theme** - idea\"\nclaims: []\ntags: []\nlinks: []\n";
    let parsed: PatternYaml = serde_yaml::from_str(raw).expect("enumeration output parses");
    assert_eq!(parsed.tldr.as_deref(), Some("The hook."));
    let enumeration = parsed
        .enumeration
        .expect("enumeration present")
        .into_enumeration()
        .expect("items");
    assert_eq!(enumeration.declared_count, Some(2));
    assert_eq!(enumeration.items.len(), 2);
    assert_eq!(enumeration.items[0].name, "A");
    assert_eq!(parsed.key_ideas.expect("key ideas").len(), 1);
}

#[test]
fn input_truncation_tag_fires_only_over_limit() {
    assert_eq!(
        input_truncation_tag(40_000, 32_000).as_deref(),
        Some("input:40000>32000")
    );
    // Exactly at the limit is not a cut.
    assert!(input_truncation_tag(32_000, 32_000).is_none());
    assert!(input_truncation_tag(100, 32_000).is_none());
    // max_chars == 0 means "no limit" (matches truncate_input's short-circuit).
    assert!(input_truncation_tag(1_000_000, 0).is_none());
}
