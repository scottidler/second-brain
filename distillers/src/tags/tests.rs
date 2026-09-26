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
    let out = dev.finish(&input(&[]), &scores);
    assert_eq!(out.confidence, Confidence::High);
    assert_eq!(out.tags, vec!["rust".to_string()]);

    let mut below = HashMap::new();
    below.insert("rust".to_string(), 0.87);
    let out = dev.finish(&input(&[]), &below);
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
        dev.finish(&input(&[]), &HashMap::new()),
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

/// Implementation audit r1, M3 and design doc G6: the ceiling holds on the
/// assembled title + text, the exact string sent, not on the text alone.
#[test]
fn clip_classifier_text_bounds_the_assembled_string() {
    let long = "x".repeat(5_000);
    let wire = clip_classifier_text("Title", &long);
    assert_eq!(wire.chars().count(), CLASSIFIER_TEXT_CHARS);
    assert!(
        wire.starts_with("Title\n\nxxx"),
        "the wire text is title + text head: {wire:.20}"
    );
}

#[test]
fn clip_classifier_text_leaves_short_text_and_multibyte_alone() {
    assert_eq!(clip_classifier_text("T", "A short body."), "T\n\nA short body.");

    // Char-wise, not byte-wise: a 3-byte char at the boundary must not panic.
    let multibyte = "\u{4e16}".repeat(CLASSIFIER_TEXT_CHARS + 10);
    assert_eq!(
        clip_classifier_text("T", &multibyte).chars().count(),
        CLASSIFIER_TEXT_CHARS
    );
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
/// can be thousands of notes long. With labels this cheap the token budget
/// never binds, so the text ceiling is what splits.
#[test]
fn the_text_chunk_ceiling_is_the_public_tier_limit() {
    assert_eq!(MAX_TEXTS_PER_CALL, 200);
    let labels = vec!["ai".to_string()];
    assert_eq!(
        plan_chunks(&[10; 451], &labels),
        vec![200, 200, 51],
        "451 texts must go out as three calls"
    );
}

/// Phase 0: a 159-note bare-label call returned 402 and 53 notes passed. The
/// budget comes from the largest pass, so a full-vocabulary shard of
/// production-sized wire text chunks well under 159 and every chunk's
/// estimate stays inside it.
#[test]
fn a_production_shard_chunks_under_the_token_budget() {
    let shard: Vec<String> = (0..59).map(|i| format!("tag-{i:04}")).collect();
    let lengths = vec![715usize; 400];
    let chunks = plan_chunks(&lengths, &shard);
    assert_eq!(chunks.iter().sum::<usize>(), 400, "every note is planned once");
    for size in &chunks {
        assert!(*size < 159, "chunk of {size} would repeat the measured 402");
        assert!(*size as f64 * note_tokens(715, &shard) <= REQUEST_TOKEN_BUDGET);
    }
    assert!(chunks[0] >= 53, "the measured passing chunk must still fit: {chunks:?}");
}

/// A chunk always holds at least one note: a note over budget on its own is
/// sent alone, and the service's 402 is what decides it.
#[test]
fn an_over_budget_note_still_goes_out_alone() {
    let labels = vec!["ai".to_string()];
    assert!(note_tokens(1_000_000, &labels) > REQUEST_TOKEN_BUDGET);
    assert_eq!(plan_chunks(&[1_000_000, 1_000_000], &labels), vec![1, 1]);
    assert_eq!(plan_chunks(&[], &labels), Vec::<usize>::new());
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

// ------------------------------------------------------------ score_batch

/// A fake classifier.dev: scores every label it is sent at 0.95 (or 0.10 for
/// labels in `low`), answers as Jev, and records the inputs of every call.
#[derive(Default)]
struct FakeService {
    /// 402 `request_spending_limit` on any call with more inputs than this.
    max_inputs: Option<usize>,
    /// 402 `request_spending_limit` on any call whose inputs contain this.
    poison: Option<String>,
    /// For an input containing `.0`, leave out the score for label `.1`.
    drop_label: Option<(String, String)>,
    /// For an input containing `.0`, add a score for the unsent label `.1`.
    extra_label: Option<(String, String)>,
    /// 500 on any call whose labels include this one.
    fail_label: Option<String>,
    low: Vec<String>,
    sent: std::sync::Mutex<Vec<Vec<String>>>,
}

const SPEND_402: &str = r#"{"error":{"code":"request_spending_limit","message":"Split the batch, shorten input text, labels or instructions"}}"#;

impl Transport for FakeService {
    fn send(&self, body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
        let inputs: Vec<String> = body["inputs"]
            .as_array()
            .expect("inputs")
            .iter()
            .map(|v| v.as_str().expect("input").to_string())
            .collect();
        let labels: Vec<String> = body["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .map(|v| v.as_str().expect("label").to_string())
            .collect();
        self.sent.lock().expect("lock").push(inputs.clone());

        if self.max_inputs.is_some_and(|max| inputs.len() > max)
            || self
                .poison
                .as_ref()
                .is_some_and(|p| inputs.iter().any(|i| i.contains(p)))
        {
            return Ok((402, SPEND_402.to_string()));
        }
        if self.fail_label.as_ref().is_some_and(|f| labels.contains(f)) {
            return Ok((500, r#"{"error":"upstream"}"#.to_string()));
        }

        let results: Vec<serde_json::Value> = inputs
            .iter()
            .map(|input| {
                let mut scores = serde_json::Map::new();
                for label in &labels {
                    let dropped = self
                        .drop_label
                        .as_ref()
                        .is_some_and(|(marker, l)| input.contains(marker) && l == label);
                    if !dropped {
                        let score = if self.low.contains(label) { 0.10 } else { 0.95 };
                        scores.insert(label.clone(), serde_json::json!(score));
                    }
                }
                if let Some((marker, extra)) = &self.extra_label
                    && input.contains(marker)
                {
                    scores.insert(extra.clone(), serde_json::json!(0.5));
                }
                serde_json::json!({"model": "jev-1.13.0", "scores": scores})
            })
            .collect();
        let tokens = 1_000 * inputs.len() as u64;
        let reply = serde_json::json!({
            "results": results,
            "usage": {"input_tokens": tokens},
            "pricing": {"estimated_usd": tokens as f64 * 0.042 / 1_000_000.0},
        });
        Ok((200, reply.to_string()))
    }
}

impl FakeService {
    fn calls(&self) -> Vec<Vec<String>> {
        self.sent.lock().expect("lock").clone()
    }
}

fn dev_over(canon: CanonicalSet, service: Arc<FakeService>) -> ClassifierDev {
    ClassifierDev::with_transport(canon, mapping(), &TagsClassifierConfig::default(), service)
}

/// A vocabulary past `MAX_LABELS_PER_CALL`, so it shards in two like
/// production's 117.
fn two_shard_canon() -> CanonicalSet {
    let mut c = canon();
    c.all = (0..117).map(|i| format!("tag-{i:03}")).collect();
    c
}

fn owned_inputs(n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| (format!("Note {i}"), format!("Summary of note {i}.")))
        .collect()
}

fn as_inputs(owned: &[(String, String)]) -> Vec<TagInput<'_>> {
    owned
        .iter()
        .map(|(title, text)| TagInput {
            title,
            text,
            candidates: &[],
        })
        .collect()
}

/// Success criterion 1: the 129,928-char summary measured on oracle.db (the
/// usage report) went out whole. The string handed to `post` is now bounded.
#[test]
fn the_string_posted_for_a_huge_summary_is_clipped() {
    let service = Arc::new(FakeService::default());
    let dev = dev_over(canon(), service.clone());
    let title = "t".repeat(200);
    let summary = "s".repeat(129_928);
    let batch = dev.score_batch(&[TagInput {
        title: &title,
        text: &summary,
        candidates: &[],
    }]);
    assert!(batch.notes[0].is_ok(), "{:?}", batch.notes[0]);

    let calls = service.calls();
    assert!(!calls.is_empty());
    for inputs in &calls {
        for wire in inputs {
            assert!(
                wire.chars().count() <= CLASSIFIER_TEXT_CHARS,
                "posted {} chars",
                wire.chars().count()
            );
            assert!(wire.starts_with(&title), "the title leads the wire text");
        }
    }
}

/// Success criterion 2a: a service that 402s above 10 notes still scores a
/// 40-note batch, by halving, and every call it accepted held at most 10.
#[test]
fn a_402_splits_the_chunk_until_it_passes() {
    let service = Arc::new(FakeService {
        max_inputs: Some(10),
        ..FakeService::default()
    });
    let dev = dev_over(canon(), service.clone());
    let owned = owned_inputs(40);
    let batch = dev.score_batch(&as_inputs(&owned));

    assert_eq!(batch.notes.len(), 40);
    assert!(batch.notes.iter().all(Result::is_ok), "every note should score");
    let accepted: Vec<usize> = service.calls().iter().map(Vec::len).filter(|n| *n <= 10).collect();
    assert_eq!(accepted.iter().sum::<usize>(), 40, "every note scored exactly once");
    assert_eq!(
        batch.calls.iter().filter(|c| c.ok).count(),
        accepted.len(),
        "one ok usage row per accepted call"
    );
    assert!(batch.calls.iter().any(|c| !c.ok), "the refused calls are recorded too");
}

/// Success criterion 2b: a note the service refuses even alone is that
/// note's `Err`; the other 39 still score.
#[test]
fn a_note_that_402s_alone_fails_only_itself() {
    let service = Arc::new(FakeService {
        max_inputs: Some(10),
        poison: Some("note 17.".to_string()),
        ..FakeService::default()
    });
    let dev = dev_over(canon(), service);
    let owned = owned_inputs(40);
    let batch = dev.score_batch(&as_inputs(&owned));

    for (i, note) in batch.notes.iter().enumerate() {
        if i == 17 {
            let err = note.as_ref().expect_err("the poisoned note must fail");
            assert!(err.to_string().contains("per-request spend limit"), "{err}");
        } else {
            assert!(note.is_ok(), "note {i} failed: {note:?}");
        }
    }
}

/// Success criterion 3a: a response missing one sent label fails that note,
/// not its neighbours, and never reads as a zero score.
#[test]
fn a_missing_label_fails_that_note_only() {
    let service = Arc::new(FakeService {
        drop_label: Some(("note 2.".to_string(), "rust".to_string())),
        ..FakeService::default()
    });
    let dev = dev_over(canon(), service);
    let owned = owned_inputs(4);
    let batch = dev.score_batch(&as_inputs(&owned));

    let err = batch.notes[2].as_ref().expect_err("the short note must fail");
    assert!(err.to_string().contains("\"rust\""), "{err}");
    for i in [0, 1, 3] {
        let scores = batch.notes[i].as_ref().expect("neighbour scores");
        assert_eq!(scores.len(), canon().all.len(), "every slug is scored");
    }
}

#[test]
fn an_unknown_returned_key_fails_that_note_only() {
    let service = Arc::new(FakeService {
        extra_label: Some(("note 1.".to_string(), "golang".to_string())),
        ..FakeService::default()
    });
    let dev = dev_over(canon(), service);
    let owned = owned_inputs(3);
    let batch = dev.score_batch(&as_inputs(&owned));

    let err = batch.notes[1].as_ref().expect_err("an unsent key must fail the note");
    assert!(err.to_string().contains("golang"), "{err}");
    assert!(batch.notes[0].is_ok() && batch.notes[2].is_ok());
}

/// Success criterion 3b: the first shard succeeded, the second failed. The
/// note is `Err`, not `Ok` with half a vocabulary of scores.
#[test]
fn a_failed_second_shard_is_err_not_partial() {
    let canon = two_shard_canon();
    let mut sorted: Vec<String> = canon.all.iter().cloned().collect();
    sorted.sort();
    let shards = shard_labels(&sorted);
    assert_eq!(shards.len(), 2);

    let service = Arc::new(FakeService {
        fail_label: Some(shards[1][0].clone()),
        ..FakeService::default()
    });
    let dev = dev_over(canon.clone(), service);
    let owned = owned_inputs(3);
    let batch = dev.score_batch(&as_inputs(&owned));
    for note in &batch.notes {
        let err = note.as_ref().expect_err("a second-shard failure must fail the note");
        assert!(err.to_string().contains("500"), "{err}");
    }
    assert_eq!(batch.calls.iter().map(|c| c.ok).collect::<Vec<_>>(), vec![true, false]);

    // The same, for one note: its second-shard answer is short a label.
    let service = Arc::new(FakeService {
        drop_label: Some(("note 1.".to_string(), shards[1][5].clone())),
        ..FakeService::default()
    });
    let dev = dev_over(canon.clone(), service);
    let batch = dev.score_batch(&as_inputs(&owned));
    assert!(batch.notes[1].is_err(), "a short second shard must fail the note");
    for i in [0, 2] {
        assert_eq!(
            batch.notes[i].as_ref().expect("scored").len(),
            117,
            "both shards merged"
        );
    }
}

/// A note that failed the first shard is not sent (or paid for) again in the
/// second.
#[test]
fn a_note_failed_in_one_shard_is_not_sent_to_the_next() {
    let canon = two_shard_canon();
    let mut sorted: Vec<String> = canon.all.iter().cloned().collect();
    sorted.sort();
    let first = shard_labels(&sorted)[0][0].clone();
    let service = Arc::new(FakeService {
        drop_label: Some(("note 0.".to_string(), first)),
        ..FakeService::default()
    });
    let dev = dev_over(canon, service.clone());
    let owned = owned_inputs(2);
    let batch = dev.score_batch(&as_inputs(&owned));
    assert!(batch.notes[0].is_err() && batch.notes[1].is_ok());
    let calls = service.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].len(), 1, "only the surviving note goes to shard two");
}

#[test]
fn call_usage_comes_from_the_response() {
    let service = Arc::new(FakeService::default());
    let dev = dev_over(canon(), service);
    let owned = owned_inputs(3);
    let batch = dev.score_batch(&as_inputs(&owned));
    assert_eq!(batch.calls.len(), 1);
    assert_eq!(batch.calls[0].input_tokens, 3_000);
    assert!((batch.calls[0].estimated_usd - 0.000126).abs() < 1e-12);
    assert!(batch.calls[0].ok);
}

/// The P0 call log's shape (`calls.jsonl`, 2026-09-26): usage and pricing are
/// read, and a response without them (the 2026-09-20 fixture) still parses.
#[test]
fn usage_and_pricing_parse_from_a_live_response_shape() {
    let live: ClassifyResponse = serde_json::from_str(
        r#"{"results":[],"usage":{"input_tokens":100663,"output_tokens":64567,"classifications":53},
            "pricing":{"currency":"USD","input_tokens":100663,"estimated_usd":0.004227846,"total_usd":0}}"#,
    )
    .expect("parses");
    let usage = live.call_usage();
    assert_eq!(usage.input_tokens, 100_663);
    assert!((usage.estimated_usd - 0.004227846).abs() < 1e-12);

    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture parses");
    let old: ClassifyResponse = serde_json::from_value(fixture["responses"]["a"].clone()).expect("parses");
    assert_eq!(old.call_usage().input_tokens, 0);
}

/// `classify_batch` used to fail every input when one call failed. Now one
/// note's failure is that note's `Err`, and the rest select tags as usual.
#[test]
fn classify_batch_fails_per_note() {
    let service = Arc::new(FakeService {
        drop_label: Some(("note 1.".to_string(), "rust".to_string())),
        low: canon().all.iter().filter(|t| *t != "rust").cloned().collect(),
        ..FakeService::default()
    });
    let dev = dev_over(canon(), service);
    let owned = owned_inputs(3);
    let out = dev.classify_batch(&as_inputs(&owned));
    assert!(out[1].is_err());
    for i in [0, 2] {
        let tags = &out[i].as_ref().expect("classified").tags;
        assert_eq!(tags, &vec!["rust".to_string()]);
    }
}

/// A day-cap 402 (or any 402 not naming the per-request limit) is not split:
/// halving cannot fix it, so the chunk fails as it stands.
#[test]
fn a_402_that_is_not_the_request_limit_does_not_split() {
    struct DayCap(std::sync::Mutex<usize>);
    impl Transport for DayCap {
        fn send(&self, _body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
            *self.0.lock().expect("lock") += 1;
            Ok((402, r#"{"error":{"code":"daily_spending_limit"}}"#.to_string()))
        }
    }
    let service = Arc::new(DayCap(std::sync::Mutex::new(0)));
    let dev = ClassifierDev::with_transport(canon(), mapping(), &TagsClassifierConfig::default(), service.clone());
    let owned = owned_inputs(8);
    let batch = dev.score_batch(&as_inputs(&owned));
    assert!(batch.notes.iter().all(Result::is_err));
    assert_eq!(*service.0.lock().expect("lock"), 1, "no retry into the cap");
}
