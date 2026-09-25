use super::*;

const FIXTURE: &str = include_str!("fixtures/classifier-dev-2026-09-20.json");

fn canon() -> CanonicalSet {
    let all = [
        "ai",
        "llm",
        "privacy",
        "rust",
        "cli",
        "programming",
        "security",
        "github",
        "devops",
        "git",
        "software-engineering",
        "work",
        "life",
        "homelab",
        "diy",
        "writing",
        "tech",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let no_segment = ["work", "life", "homelab", "diy", "tech"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    CanonicalSet {
        all,
        no_segment,
        no_classifier: protect_list(),
        max_per_note: 8,
    }
}

fn mapping() -> TagMapping {
    let mut m = TagMapping::new();
    m.insert("ai-llm".to_string(), Some("llm".to_string()));
    m.insert("golang".to_string(), None);
    m
}

fn input<'a>(candidates: &'a [TagCandidate]) -> TagInput<'a> {
    TagInput {
        title: "A note",
        text: "Body text that the deterministic impl never reads.",
        candidates,
    }
}

fn protect_list() -> HashSet<String> {
    ["work", "life", "homelab", "diy", "writing"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// ------------------------------------------------------------ invariants

#[test]
fn deterministic_is_a_pure_function() {
    // 20 candidates covering every tier and both ignorable sources.
    let raw = [
        "rust",
        "ai-llm",
        "cli",
        "golang",
        "privacy",
        "rust-cli-tooling",
        "work-life-balance",
        "security",
        "github",
        "devops",
        "git",
        "programming",
        "software-engineering",
        "unknown-junk",
        "ai",
        "llm",
        "writing",
        "homelab",
        "diy",
        "tech",
    ];
    let candidates: Vec<TagCandidate> = raw
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let source = match i % 3 {
                0 => CandidateSource::Author,
                1 => CandidateSource::Model,
                _ => CandidateSource::Preserved,
            };
            TagCandidate::new(*t, source)
        })
        .collect();

    let classifier = Deterministic::new(canon(), mapping());
    let first = classifier.classify(&input(&candidates)).expect("classify");
    let baseline = format!("{:?}|{:?}", first.tags, first.confidence);

    for run in 1..10 {
        let next = classifier.classify(&input(&candidates)).expect("classify");
        assert_eq!(
            baseline,
            format!("{:?}|{:?}", next.tags, next.confidence),
            "run {run} differed from the first"
        );
    }
}

#[test]
fn outputs_are_canonical_and_capped() {
    let c = canon();
    let m = mapping();

    // The fake is handed junk and more tags than the cap allows.
    let canned: Vec<String> = [
        "rust",
        "cli",
        "ai",
        "llm",
        "privacy",
        "security",
        "github",
        "devops",
        "git",
        "programming",
        "not-a-real-tag",
        "golang",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let fake = FakeTagClassifier::new(c.clone(), m.clone(), canned);
    let out = fake.classify(&input(&[])).expect("classify");
    assert!(out.tags.len() <= c.max_per_note, "cap breached: {:?}", out.tags);
    for tag in &out.tags {
        assert!(c.all.contains(tag), "non-canonical tag {tag} escaped");
    }

    // The recorded response upholds the same two invariants.
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    let cap = fixture["max_per_note"].as_u64().expect("max_per_note") as usize;
    let vocab: HashSet<String> = fixture["shards"]["a"]
        .as_array()
        .expect("shard a")
        .iter()
        .chain(fixture["shards"]["b"].as_array().expect("shard b"))
        .map(|v| v.as_str().expect("label").to_string())
        .collect();
    for expected in fixture["expected"].as_array().expect("expected") {
        let tags = expected["tags"].as_array().expect("tags");
        assert!(tags.len() <= cap, "recorded response breaches the cap");
        for tag in tags {
            let tag = tag.as_str().expect("tag");
            assert!(vocab.contains(tag), "recorded tag {tag} is outside the vocabulary");
        }
    }
}

#[test]
fn model_candidates_are_ignored_by_repeatable_impls() {
    let c = canon();
    let m = mapping();

    // A Fabric run that returns nothing: whatever survives came from the
    // candidates, not the model.
    struct EmptyReply;
    impl FabricRunner for EmptyReply {
        fn run(&self, _pattern: &str, _input: &str) -> Result<String> {
            Ok(r#"{"tags": []}"#.to_string())
        }
    }
    let fabric = FabricClosedVocab::new(c.clone(), m.clone(), Arc::new(EmptyReply));

    let model_only = [TagCandidate::new("rust", CandidateSource::Model)];
    let out = fabric.classify(&input(&model_only)).expect("classify");
    assert!(out.tags.is_empty(), "Model candidate leaked in: {:?}", out.tags);

    let preserved_only = [TagCandidate::new("rust", CandidateSource::Preserved)];
    let out = fabric.classify(&input(&preserved_only)).expect("classify");
    assert!(out.tags.is_empty(), "Preserved candidate leaked in: {:?}", out.tags);

    let author_only = [TagCandidate::new("rust", CandidateSource::Author)];
    let out = fabric.classify(&input(&author_only)).expect("classify");
    assert_eq!(out.tags, vec!["rust".to_string()], "Author candidate was dropped");

    // Contrast: Deterministic is the impl that consumes every source.
    let det = Deterministic::new(c, m);
    let out = det.classify(&input(&model_only)).expect("classify");
    assert_eq!(out.tags, vec!["rust".to_string()]);
}

#[test]
fn shards_are_sorted_and_merge_to_recorded_scores() {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    let read = |key: &str| -> Vec<String> {
        fixture["shards"][key]
            .as_array()
            .expect("shard")
            .iter()
            .map(|v| v.as_str().expect("label").to_string())
            .collect()
    };
    let (a, b) = (read("a"), read("b"));

    let mut vocabulary: Vec<String> = a.iter().chain(b.iter()).cloned().collect();
    vocabulary.sort();
    assert_eq!(vocabulary.len(), 117, "the P1 vocabulary is 117 tags");

    // 117 labels against a 100-label ceiling is two calls, split evenly.
    let shards = shard_labels(&vocabulary);
    assert_eq!(shards.len(), 2, "117 labels must shard into exactly two calls");
    assert_eq!(shards[0].len(), 59);
    assert_eq!(shards[1].len(), 58);
    assert_eq!(shards[0], a, "shard A drifted from the recorded partition");
    assert_eq!(shards[1], b, "shard B drifted from the recorded partition");
    for shard in &shards {
        assert!(shard.len() <= MAX_LABELS_PER_CALL);
        let mut sorted = shard.clone();
        sorted.sort();
        assert_eq!(*shard, sorted, "shard labels must be sorted");
    }

    // Merging the two recorded shard responses reproduces the recorded tags.
    let threshold = fixture["threshold"].as_f64().expect("threshold") as f32;
    let cap = fixture["max_per_note"].as_u64().expect("max_per_note") as usize;
    let expected = fixture["expected"].as_array().expect("expected");

    for (i, want) in expected.iter().enumerate() {
        let shard_scores: Vec<HashMap<String, f32>> = ["a", "b"]
            .iter()
            .map(|k| {
                fixture["responses"][k]["results"][i]["scores"]
                    .as_object()
                    .expect("scores")
                    .iter()
                    .map(|(label, v)| (label.clone(), v.as_f64().expect("score") as f32))
                    .collect()
            })
            .collect();

        let merged = merge_shard_scores(&shard_scores);
        let selected = select_above_threshold(&merged, threshold, cap);
        let got: Vec<String> = selected.into_iter().map(|(t, _)| t).collect();
        let want_tags: Vec<String> = want["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .map(|v| v.as_str().expect("tag").to_string())
            .collect();
        assert_eq!(got, want_tags, "merge differed for input {}", want["id"]);
    }
}

#[test]
fn confidence_mapping_per_method() {
    let c = canon();
    let m = mapping();

    // deterministic: tier 0/1 -> High, tier 2 only -> Medium, no hit -> Low.
    let det = Deterministic::new(c.clone(), m.clone());
    let exact = [TagCandidate::new("rust", CandidateSource::Preserved)];
    assert_eq!(det.classify(&input(&exact)).expect("c").confidence, Confidence::High);
    let mapped = [TagCandidate::new("ai-llm", CandidateSource::Preserved)];
    assert_eq!(det.classify(&input(&mapped)).expect("c").confidence, Confidence::High);
    let segment = [TagCandidate::new("rust-cli-tooling", CandidateSource::Preserved)];
    assert_eq!(
        det.classify(&input(&segment)).expect("c").confidence,
        Confidence::Medium
    );
    let nothing = [TagCandidate::new("unknown-junk", CandidateSource::Preserved)];
    assert_eq!(det.classify(&input(&nothing)).expect("c").confidence, Confidence::Low);

    // fabric-closed: never High. Tags -> Medium, none -> Low.
    struct Reply(&'static str);
    impl FabricRunner for Reply {
        fn run(&self, _pattern: &str, _input: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }
    let some = FabricClosedVocab::new(c.clone(), m.clone(), Arc::new(Reply(r#"{"tags":["rust"]}"#)));
    let out = some.classify(&input(&[])).expect("c");
    assert_eq!(out.confidence, Confidence::Medium);
    assert_ne!(out.confidence, Confidence::High);
    let none = FabricClosedVocab::new(c.clone(), m.clone(), Arc::new(Reply(r#"{"tags":[]}"#)));
    assert_eq!(none.classify(&input(&[])).expect("c").confidence, Confidence::Low);

    // classifier-dev: never Medium. At or above threshold -> High, else Low.
    let cfg = TagsClassifierConfig::default();
    let dev = ClassifierDev::new(c, m, &cfg);
    let mut scores = HashMap::new();
    scores.insert("rust".to_string(), 0.95);
    let out = dev.finish(&input(&[]), std::slice::from_ref(&scores));
    assert_eq!(out.confidence, Confidence::High);
    assert_eq!(out.tags, vec!["rust".to_string()]);

    let mut below = HashMap::new();
    below.insert("rust".to_string(), 0.87);
    let out = dev.finish(&input(&[]), &[below]);
    assert_eq!(out.confidence, Confidence::Low);
    assert_ne!(out.confidence, Confidence::Medium);
    assert!(out.tags.is_empty());
}

// ----------------------------------------------------------------- retag

#[test]
fn retag_never_removes_no_classifier_tags() {
    // A note carrying `work` (protected) and `rust` (not) is retagged against a
    // result that returns neither: `work` survives, `rust` does not.
    let previous = vec!["work".to_string(), "rust".to_string()];
    let fresh = TagOutput {
        tags: vec!["security".to_string()],
        scores: None,
        confidence: Confidence::High,
        method: TagMethod::ClassifierDev,
    };
    let got = apply_retag(&previous, &fresh, &canon());
    assert!(got.contains(&"work".to_string()), "protected tag was dropped: {got:?}");
    assert!(!got.contains(&"rust".to_string()), "unprotected tag survived: {got:?}");
    assert!(got.contains(&"security".to_string()));
}

#[test]
fn retag_replaces_tags_outside_the_protect_list() {
    let previous = vec!["rust".to_string(), "cli".to_string()];
    let fresh = TagOutput {
        tags: vec!["ai".to_string(), "llm".to_string()],
        scores: None,
        confidence: Confidence::High,
        method: TagMethod::ClassifierDev,
    };
    let got = apply_retag(&previous, &fresh, &canon());
    assert_eq!(got, vec!["ai".to_string(), "llm".to_string()], "replace, not union");
}

#[test]
fn retag_never_writes_empty() {
    let previous = vec!["rust".to_string()];

    let empty = TagOutput {
        tags: Vec::new(),
        scores: None,
        confidence: Confidence::High,
        method: TagMethod::ClassifierDev,
    };
    assert_eq!(apply_retag(&previous, &empty, &canon()), previous);

    let low = TagOutput {
        tags: vec!["ai".to_string()],
        scores: None,
        confidence: Confidence::Low,
        method: TagMethod::ClassifierDev,
    };
    assert_eq!(apply_retag(&previous, &low, &canon()), previous);
}

#[test]
fn segment_guard_holds_through_the_classifier() {
    // The P1 guard must survive the seam: `work-life-balance` never mints
    // `work` or `life`, whichever impl consumes the candidate.
    let det = Deterministic::new(canon(), mapping());
    let guarded = [TagCandidate::new("work-life-balance", CandidateSource::Preserved)];
    let out = det.classify(&input(&guarded)).expect("classify");
    assert!(out.tags.is_empty(), "segment guard leaked: {:?}", out.tags);
}

#[test]
fn a_high_result_always_carries_a_tag() {
    let c = canon();
    let m = mapping();
    let cfg = TagsClassifierConfig::default();
    let dev = ClassifierDev::new(c.clone(), m.clone(), &cfg);
    let det = Deterministic::new(c, m);

    for out in [
        dev.finish(&input(&[]), &[HashMap::new()]),
        det.classify(&input(&[])).expect("classify"),
    ] {
        if out.confidence == Confidence::High {
            assert!(!out.tags.is_empty(), "High with no tags from {:?}", out.method);
        }
    }
}

/// Implementation audit r1, M1: `apply_retag` prepended the protected tags to
/// an already-capped fresh set and never re-capped, so a note came out of
/// `--retag` over `max-per-note` (nine tags against a cap of eight on the
/// panel's fixture). The protected tag still survives, but the cap holds.
#[test]
fn retag_never_writes_over_the_cap() {
    let c = canon();
    let previous = vec!["work".to_string(), "rust".to_string()];
    let fresh = TagOutput {
        tags: [
            "ai",
            "llm",
            "privacy",
            "security",
            "github",
            "devops",
            "git",
            "programming",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect(),
        scores: None,
        confidence: Confidence::High,
        method: TagMethod::ClassifierDev,
    };
    assert_eq!(fresh.tags.len(), c.max_per_note, "the fresh set already fills the cap");

    let got = apply_retag(&previous, &fresh, &c);
    assert_eq!(got.len(), c.max_per_note, "cap breached: {got:?}");
    assert_eq!(got[0], "work", "the protected tag claims its slot first");
    assert!(!got.contains(&"rust".to_string()), "unprotected tag survived: {got:?}");
}

// -------------------------------------------------------- classifier text

/// Implementation audit r1, M3: the 900-character ceiling the design doc
/// promises had no implementation in borg, so an audio note with no summary
/// POSTed its whole transcript to classifier.dev.
#[test]
fn body_excerpt_clips_at_the_documented_ceiling() {
    let long = "x".repeat(5_000);
    assert_eq!(body_excerpt(&long).chars().count(), CLASSIFIER_TEXT_CHARS);
    assert!(long.starts_with(&body_excerpt(&long)), "the excerpt is the head");
}

#[test]
fn body_excerpt_leaves_short_text_and_multibyte_alone() {
    let short = "A short body.";
    assert_eq!(body_excerpt(short), short);

    // Char-wise, not byte-wise: a 3-byte char at the boundary must not panic.
    let multibyte = "\u{4e16}".repeat(CLASSIFIER_TEXT_CHARS + 10);
    assert_eq!(body_excerpt(&multibyte).chars().count(), CLASSIFIER_TEXT_CHARS);
}

// --- 0.15.3: keyless by default, optional workspace token ------------------

/// The shipped default is the KEYLESS public tier. `CLASSIFY_API_KEY` sat in
/// the environment and in the secrets manifest while classifier.dev rejected
/// it (401 invalid_api_key), so every classify fell back to deterministic and
/// nothing said so. Defaulting to no credential means the working path needs
/// no setup, and a stale key cannot reintroduce that state.
#[test]
fn the_default_config_names_no_token_env() {
    let cfg = TagsClassifierConfig::default();
    assert_eq!(cfg.token_env, "", "the default must be the keyless tier");
}

/// `token-env` names an env var, never a token. An empty name, or a name
/// whose variable is unset or blank, is the keyless path and NOT an error:
/// falling through to the public tier is a working request.
#[test]
fn an_unset_or_blank_token_env_resolves_keyless() {
    let c = canon();
    let m = TagMapping::default();

    let mut cfg = TagsClassifierConfig::default();
    assert!(ClassifierDev::new(c.clone(), m.clone(), &cfg).token().is_none());

    cfg.token_env = "DISTILLERS_TEST_UNSET_TOKEN_9c2f4b".to_string();
    assert!(
        ClassifierDev::new(c.clone(), m.clone(), &cfg).token().is_none(),
        "a named-but-unset variable must fall through to keyless, not error"
    );

    // SAFETY: single-threaded test, variable is uniquely named to this test.
    unsafe { std::env::set_var("DISTILLERS_TEST_BLANK_TOKEN_9c2f4b", "   ") };
    cfg.token_env = "DISTILLERS_TEST_BLANK_TOKEN_9c2f4b".to_string();
    assert!(
        ClassifierDev::new(c.clone(), m.clone(), &cfg).token().is_none(),
        "a whitespace-only value is not a token"
    );

    unsafe { std::env::set_var("DISTILLERS_TEST_REAL_TOKEN_9c2f4b", "sk-live-xyz") };
    cfg.token_env = "DISTILLERS_TEST_REAL_TOKEN_9c2f4b".to_string();
    assert_eq!(
        ClassifierDev::new(c, m, &cfg).token().as_deref(),
        Some("sk-live-xyz"),
        "a set token must still be used"
    );
    unsafe {
        std::env::remove_var("DISTILLERS_TEST_BLANK_TOKEN_9c2f4b");
        std::env::remove_var("DISTILLERS_TEST_REAL_TOKEN_9c2f4b");
    }
}

/// The public tier caps a call at 200 texts. Before the chunking the whole
/// batch went in one request, and `summarize --backfill` / `classify --retag`
/// can be thousands of notes long.
#[test]
fn the_text_chunk_ceiling_is_the_public_tier_limit() {
    assert_eq!(MAX_TEXTS_PER_CALL, 200);
    let texts: Vec<usize> = (0..451).collect();
    let chunks: Vec<usize> = texts.chunks(MAX_TEXTS_PER_CALL).map(<[usize]>::len).collect();
    assert_eq!(chunks, vec![200, 200, 51], "451 texts must go out as three calls");
}

#[test]
fn the_request_uses_the_multi_field() {
    let body = request_body(&["ai".to_string()], &["text".to_string()], 8);
    assert_eq!(body["multi"], serde_json::json!(true));
    assert!(
        body.get("multi_label").is_none(),
        "multi_label is not a classifier.dev field"
    );
    assert_eq!(body["max_labels"], serde_json::json!(8));
}

#[test]
fn only_jev_answers_are_accepted() {
    let parse = |json: &str| -> ClassifyResponse { serde_json::from_str(json).expect("parses") };

    let jev = parse(r#"{"results":[{"scores":{"ai":0.9},"model":"jev-1.13.0"},{"scores":{},"model":"jev@vercel"}]}"#);
    assert!(ensure_jev(&jev).is_ok());

    let fallback = parse(
        r#"{"results":[{"scores":{"ai":0.9},"model":"jev-1.13.0"},{"scores":{"ai":0.8},"model":"ling-3.0-flash"}]}"#,
    );
    let err = ensure_jev(&fallback).expect_err("a non-Jev answer must fail the call");
    assert!(err.to_string().contains("ling-3.0-flash"), "{err}");

    let unnamed = parse(r#"{"results":[{"scores":{"ai":0.9}}]}"#);
    assert!(
        ensure_jev(&unnamed).is_err(),
        "an answer naming no model is not proven Jev"
    );
}
