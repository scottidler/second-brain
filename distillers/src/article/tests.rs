use super::*;
use crate::FakeFabric;

fn make_distiller(fake: FakeFabric) -> ArticleDistiller<std::sync::Arc<FakeFabric>> {
    ArticleDistiller::new(std::sync::Arc::new(fake), ArticleConfig::default())
}

#[tokio::test]
async fn happy_path_parses_yaml_into_distilled() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "An article on distributed consensus. It argues that Raft is easier to teach than Paxos."
claims:
  - text: "Raft uses a leader-based approach that simplifies replication."
    anchor: null
  - text: "Paxos's vocabulary obscures its operational semantics."
    anchor: null
tags: []
links:
  - url: "https://raft.github.io"
    label: "Raft homepage"
"#,
    );
    let distiller = ArticleDistiller::new(std::sync::Arc::new(fake), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body about Raft and Paxos.",
            source_url: Some("https://example.com/raft-vs-paxos"),
            title_hint: Some("Raft vs Paxos"),
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-article-v1");
    assert!(distilled.summary.starts_with("An article on distributed consensus"));
    assert_eq!(distilled.claims.len(), 2);
    assert_eq!(
        distilled.claims[0].text,
        "Raft uses a leader-based approach that simplifies replication."
    );
    assert!(distilled.tags.is_empty());
    assert_eq!(distilled.links.len(), 1);
    assert_eq!(distilled.links[0].url, "https://raft.github.io");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Phase 7: the short single-call path now persists the fetched markdown
    // verbatim in-note (was `None` pre-Phase-7 - "origin URL is the archive").
    assert_eq!(
        distilled.transcript.as_deref(),
        Some("Article body about Raft and Paxos."),
        "short-path article must carry its fetched markdown under ## Transcript"
    );
}

#[tokio::test]
async fn fabric_timeout_falls_back() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN);
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    // meta.model carries the real model (empty config -> "default"), NOT the
    // failure reason - that lives in validation.fallback_reason above.
    assert_eq!(distilled.meta.model, "default");
    assert!(distilled.summary.starts_with("[fabric-timeout]"));
    assert!(distilled.claims.is_empty());
}

#[tokio::test]
async fn fabric_error_falls_back_with_error_reason() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-article failed: 1");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-error")
    );
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml: [unclosed");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("yaml-parse-error")
    );
    assert!(
        distilled.meta.validation.raw_output.is_some(),
        "raw_output must be preserved for forensics"
    );
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "summary: \"\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Article body text.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("missing-summary")
    );
}

#[tokio::test]
async fn strips_yaml_code_fence_if_present() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"Fenced response\"\nclaims: []\ntags: []\nlinks: []\n```\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "Fenced response");
}

#[tokio::test]
async fn strips_bare_code_fence_if_present() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```\nsummary: \"Bare fenced\"\nclaims: []\ntags: []\nlinks: []\n```",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "Bare fenced");
}

#[tokio::test]
async fn truncates_excess_claims_via_enforce_bounds() {
    let fake = FakeFabric::new();
    let mut yaml = String::from("summary: \"S\"\nclaims:\n");
    for i in 0..15 {
        yaml.push_str(&format!("  - text: \"Claim {i}\"\n    anchor: null\n"));
    }
    yaml.push_str("tags: []\nlinks: []\n");
    fake.set_response(PATTERN, yaml);

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.claims.len(), crate::validate::max_claims(1));
    assert!(
        distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("claims:"))
    );
}

#[tokio::test]
async fn drops_empty_claim_texts_and_anchors() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S\"\nclaims:\n  - text: \"   \"\n    anchor: \"\"\n  - text: \"Real claim.\"\n    anchor: \"\"\ntags: []\nlinks: []\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.claims.len(), 1);
    assert_eq!(distilled.claims[0].text, "Real claim.");
    assert!(distilled.claims[0].anchor.is_none());
}

#[tokio::test]
async fn lowercases_tag_strings() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S\"\nclaims: []\ntags: [\"Rust\", \"DistributedSystems\"]\nlinks: []\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.tags, vec!["rust", "distributedsystems"]);
}

#[tokio::test]
async fn records_request_pattern_in_fake_history() {
    let fake = FakeFabric::new();
    let fake = std::sync::Arc::new(fake);
    fake.set_response(PATTERN, "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].pattern, PATTERN);
}

// ---- Phase 6: map-reduce long path ----

const UNIQUE_FACT: &str = "The Zorblax coefficient measured forty-two across every independent trial.";

/// Build an article transcript above the long-path threshold (>48K chars) with
/// a unique fact placed only BEYOND the 32K-char (max_chars) mark, so a
/// single-call distiller would have truncated it away.
fn article_with_late_fact() -> String {
    let filler = "This paragraph discusses distributed consensus, replication, and quorum protocols in depth. ";
    let mut transcript = String::new();
    while transcript.len() < 40_000 {
        transcript.push_str(filler);
    }
    transcript.push_str(UNIQUE_FACT);
    transcript.push(' ');
    while transcript.len() < 60_000 {
        transcript.push_str(filler);
    }
    transcript
}

#[tokio::test]
async fn short_article_stays_on_single_call_path() {
    // A sub-threshold transcript issues exactly one call, to the single-call
    // pattern - the long path never perturbs sub-threshold behavior.
    let transcript = "A short article body about Raft and Paxos.";
    assert!(approx_tokens(transcript.len()) <= SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"Short article summary.\"\nclaims:\n  - text: \"A single claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    let calls = fake.calls();
    assert_eq!(calls.len(), 1, "single-call path issues exactly one fabric call");
    assert_eq!(calls[0].pattern, PATTERN, "and it is the single-call pattern");
    assert_eq!(distilled.summary, "Short article summary.");
    assert!(
        !distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("input:")),
        "a sub-threshold input that fits max_chars records no truncation"
    );
}

#[tokio::test]
async fn long_article_covers_whole_input_with_zero_truncation() {
    // HARD GATE: a >32K article distills via the long path with (1) zero
    // truncate_input cuts, (2) every chunk mapped exactly once (full coverage),
    // and (3) the fact-bearing chunk (beyond the 32K mark) present in the map
    // set - never truncated away.
    let transcript = article_with_late_fact();
    assert!(
        approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD,
        "fixture must route to the long path"
    );
    let fact_offset = transcript.find(UNIQUE_FACT).expect("fact present");
    assert!(
        fact_offset > 32_000,
        "the unique fact must sit beyond the 32K-char mark"
    );

    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"A chunk-level claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Synthesized whole-article summary.\"\nclaims:\n  - text: \"A selected synthesis claim.\"\n    anchor: null\n",
    );
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://example.com/long-essay"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    // (1) zero truncate_input cuts on the long path.
    assert!(
        !distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("input:")),
        "long path must not truncate the input: {:?}",
        distilled.meta.validation.bounds_truncations
    );

    // (2) + (3) full coverage: the multiset of chunk inputs equals the shared
    // chunker's output, and one of them carries the late fact.
    let chunk_inputs: Vec<String> = fake
        .calls()
        .into_iter()
        .filter(|c| c.pattern == PATTERN_CHUNK)
        .map(|c| c.input)
        .collect();
    let expected = chunk_transcript(&transcript, CHUNK_TOKEN_TARGET);
    assert!(expected.len() > 1, "the fixture must span multiple chunks");
    let mut got = chunk_inputs.clone();
    got.sort();
    let mut exp = expected.clone();
    exp.sort();
    assert_eq!(
        got, exp,
        "every chunk mapped exactly once - full coverage, no truncation"
    );
    assert!(
        chunk_inputs.iter().any(|i| i.contains(UNIQUE_FACT)),
        "the fact-bearing chunk (beyond 32K) must reach the map set"
    );

    assert!(distilled.meta.validation.fallback_reason.is_none());
    assert_eq!(distilled.summary, "Synthesized whole-article summary.");
    // Phase 7: the long (map-reduce) path persists the FULL fetched markdown
    // in-note too - not just the short path - so a chunked essay is as durable
    // as a short one past staging retention.
    assert_eq!(
        distilled.transcript.as_deref(),
        Some(transcript.as_str()),
        "long-path article must carry the full fetched markdown under ## Transcript"
    );
}

#[tokio::test]
async fn long_article_reduce_selected_fact_appears_in_published_claims() {
    // The reduce selects a claim carrying the late fact; it survives into the
    // published claims (the reduce->publish plumbing carries a fact-bearing
    // claim). The LIVE proof that the model itself selects the fact is a
    // pending operator replay - this asserts the deterministic half.
    let transcript = article_with_late_fact();
    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"A chunk-level claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        format!("summary: \"Whole-article summary.\"\nclaims:\n  - text: \"{UNIQUE_FACT}\"\n    anchor: null\n"),
    );
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert!(
        distilled.claims.iter().any(|c| c.text.contains("Zorblax")),
        "the fact-bearing selected claim must appear in the published claims: {:?}",
        distilled.claims
    );
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Articles carry no anchors, so no anchor is stripped even though the pool
    // is anchorless.
    assert_eq!(distilled.meta.validation.anchors_stripped, 0);
}

#[tokio::test]
async fn long_article_reduce_failure_falls_back_to_chronological() {
    // A failed reduce call reverts claims to the chronological chunk merge and
    // records the distinct reduce-selection-failed reason (never folded into
    // bounds_truncations).
    let transcript = article_with_late_fact();
    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"A chronological claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_error(PATTERN_REDUCE, "reduce boom");
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("reduce-selection-failed")
    );
    assert!(!distilled.claims.is_empty(), "chronological merge claims survive");
    assert!(
        !distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.contains("reduce-selection")),
        "the fallback reason is distinct from bounds_truncations"
    );
    assert!(
        distilled.summary.contains("Chunk summary text."),
        "summary falls back to concatenated chunk summaries"
    );
}

#[tokio::test]
async fn long_article_builds_two_section_reduce_input() {
    let transcript = article_with_late_fact();
    let fake = std::sync::Arc::new(FakeFabric::new());
    // Realistic per-chunk density (a 2-sentence summary + the pattern's max of 5
    // claims, each near the ~120-char norm) so the measured reduce-input size
    // reflects a real long-path fixture, not a trivial mock.
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"This chunk covers quorum negotiation and how replicas reconcile divergent logs. It also introduces the leader-lease optimization used to cut read latency.\"\nclaims:\n  - text: \"Pooled article claim about how quorum negotiation bounds the divergence window between replicas.\"\n    anchor: null\n  - text: \"Leader leases let a stable leader serve linearizable reads without a round trip to the quorum.\"\n    anchor: null\n  - text: \"Log reconciliation replays the missing suffix rather than snapshotting the whole state machine.\"\n    anchor: null\n  - text: \"Membership changes use joint consensus so no single configuration can split the cluster.\"\n    anchor: null\n  - text: \"Backpressure from a slow follower is isolated so it cannot stall the commit index for the majority.\"\n    anchor: null\ntags: [distributed-systems, consensus]\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced.\"\nclaims:\n  - text: \"Pooled article claim about how quorum negotiation bounds the divergence window between replicas.\"\n    anchor: null\n",
    );
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    let reduce_call = fake
        .calls()
        .into_iter()
        .find(|c| c.pattern == PATTERN_REDUCE)
        .expect("reduce call recorded");
    assert!(reduce_call.input.contains("## Chunk Summaries"));
    assert!(reduce_call.input.contains("## Claim Pool"));
    // Anchorless article pool lines are plain text (no `[HH:MM:SS]` prefix).
    assert!(reduce_call.input.contains("Pooled article claim about"));
    assert!(
        !reduce_call.input.contains("] Pooled article claim about"),
        "article pool lines carry no anchor bracket"
    );
    // Measurement for the impl notes (lost-in-the-middle budget check).
    eprintln!(
        "PHASE6-MEASURE article reduce-input: {} chars (~{} tokens), {} chunks",
        reduce_call.input.chars().count(),
        approx_tokens(reduce_call.input.len()),
        chunk_transcript(&transcript, CHUNK_TOKEN_TARGET).len(),
    );
}

#[tokio::test]
async fn sub_threshold_oversize_input_records_loud_truncation() {
    // A single-call input longer than max_chars would be silently cut by
    // vault::fabric::truncate_input. The distiller detects the cut at its
    // boundary and records a distinct `input:` bounds_truncations entry (not a
    // reduce-selection-failed, not a silent log-only WARN).
    let filler = "Sentence about consensus and replication protocols in distributed systems. ";
    let mut transcript = String::new();
    while transcript.len() < 40_000 {
        transcript.push_str(filler);
    }
    // Single-call (tokens <= 12K) yet over max_chars (32K).
    assert!(approx_tokens(transcript.len()) <= SINGLE_CALL_TOKEN_THRESHOLD);
    assert!(transcript.chars().count() > ArticleConfig::default().max_chars);

    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"Summary of an oversize single-call article.\"\nclaims:\n  - text: \"A claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    let distiller = ArticleDistiller::new(fake.clone(), ArticleConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://example.com/oversize"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    let char_count = transcript.chars().count();
    let expected_tag = format!("input:{char_count}>{}", ArticleConfig::default().max_chars);
    assert!(
        distilled.meta.validation.bounds_truncations.contains(&expected_tag),
        "loud truncation entry expected, got {:?}",
        distilled.meta.validation.bounds_truncations
    );
    // Distinct from the reduce-selection-failed signal.
    assert!(distilled.meta.validation.fallback_reason.is_none());
}

#[tokio::test]
async fn single_call_article_populates_enumeration_and_strips_item_anchors() {
    // Phase 4: an awesome-list article yields the enumeration; article items
    // carry no honest anchor, so any anchor the model emits is stripped.
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"A curated list of tools.\"\n\
         tldr: \"Two tools worth bookmarking.\"\n\
         enumeration:\n  lead_in: \"Two tools:\"\n  declared_count: 2\n  items:\n\
         \x20   - name: \"Alpha\"\n      text: \"first\"\n      anchor: \"00:01:00\"\n\
         \x20   - name: \"Bravo\"\n      text: \"second\"\n      anchor: null\n\
         key_ideas:\n  - \"**Curation** - hand-picked beats exhaustive\"\n\
         claims: []\ntags: []\nlinks: []\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "An awesome list of two tools: Alpha and Bravo.",
            source_url: Some("https://example.com/awesome"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.tldr.as_deref(), Some("Two tools worth bookmarking."));
    let enumeration = distilled.enumeration.expect("enumeration populated");
    assert_eq!(enumeration.declared_count, Some(2));
    let names: Vec<&str> = enumeration.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Bravo"]);
    assert!(
        enumeration.items.iter().all(|i| i.anchor.is_none()),
        "article item anchors stripped (no honest positional anchor for prose)"
    );
    assert_eq!(distilled.key_ideas.len(), 1);
    assert!(!distilled.meta.validation.enumeration_shortfall);
}
