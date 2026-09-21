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

// ------------------------------------------------------- ranking at the cap

/// Implementation audit fold, A8: `finish` truncated to the cap and THEN
/// dropped unknown labels, so a service echoing a label outside the
/// vocabulary inside its top N evicted a valid tag and the note landed one
/// short. Eight valid labels clear the threshold here; all eight must land.
#[test]
fn an_echoed_unknown_label_never_evicts_a_valid_tag() {
    let c = canon();
    let dev = ClassifierDev::new(c.clone(), mapping(), &TagsClassifierConfig::default());
    let valid = ["ai", "llm", "privacy", "rust", "cli", "security", "github", "devops"];
    assert_eq!(valid.len(), c.max_per_note);
    let mut scores: HashMap<String, f32> = valid.iter().map(|t| (t.to_string(), 0.95)).collect();
    scores.insert("not-in-the-vocabulary".to_string(), 0.99);

    let out = dev.finish(&input(&[]), &[scores]);
    assert_eq!(out.tags.len(), c.max_per_note, "a valid tag was evicted: {:?}", out.tags);
    for tag in &valid {
        assert!(out.tags.contains(&tag.to_string()), "{tag} missing from {:?}", out.tags);
    }
    assert!(!out.tags.iter().any(|t| t == "not-in-the-vocabulary"));
}

/// Implementation audit fold, A12: the unscored impls sorted author and model
/// tags together alphabetically before capping, so an author (tier 0) tag
/// lost its slot to an alphabetically earlier model tag. Eight model tags
/// that all sort before `writing` fill the cap; `writing` must still land.
#[test]
fn author_tags_outrank_model_tags_at_the_cap() {
    let c = canon();
    let m = mapping();
    let eight_model_tags = ["ai", "cli", "devops", "git", "github", "llm", "privacy", "programming"];
    assert_eq!(eight_model_tags.len(), c.max_per_note);
    let author = [TagCandidate::new("writing", CandidateSource::Author)];

    struct Reply(String);
    impl FabricRunner for Reply {
        fn run(&self, _pattern: &str, _input: &str) -> Result<String> {
            Ok(self.0.clone())
        }
    }
    let reply = serde_json::json!({ "tags": eight_model_tags }).to_string();
    let fabric = FabricClosedVocab::new(c.clone(), m.clone(), Arc::new(Reply(reply)));
    let out = fabric.classify(&input(&author)).expect("classify");
    assert_eq!(out.tags.len(), c.max_per_note, "cap breached: {:?}", out.tags);
    assert_eq!(out.tags[0], "writing", "the author tag must rank first: {:?}", out.tags);

    let canned: Vec<String> = eight_model_tags.iter().map(|s| s.to_string()).collect();
    let fake = FakeTagClassifier::new(c.clone(), m, canned);
    let out = fake.classify(&input(&author)).expect("classify");
    assert_eq!(out.tags.len(), c.max_per_note, "cap breached: {:?}", out.tags);
    assert_eq!(out.tags[0], "writing", "the author tag must rank first: {:?}", out.tags);
}

// ------------------------------------------------------------ build_kind

#[test]
fn fabric_closed_builds_the_fabric_impl_only_with_a_runner() {
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::FabricClosed,
        ..TagsClassifierConfig::default()
    };
    // No runner: the documented degrade to the impl that needs nothing.
    assert_eq!(build(&cfg, &canon(), &mapping(), None).method(), TagMethod::Deterministic);

    struct Never;
    impl FabricRunner for Never {
        fn run(&self, _pattern: &str, _input: &str) -> Result<String> {
            Err(eyre!("never called"))
        }
    }
    let runner: Arc<dyn FabricRunner> = Arc::new(Never);
    assert_eq!(build(&cfg, &canon(), &mapping(), Some(runner)).method(), TagMethod::FabricClosed);
}

// ------------------------------------------------------ classifier.dev HTTP

/// A loopback HTTP/1.1 server for the negative paths: no network, no new
/// dependency. Each accepted connection reads one request, hands the parsed
/// JSON body to `respond`, and writes the `(status, body)` it returns, or
/// holds the connection open without answering when it returns `None`.
mod http {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Once;
    use std::time::Duration;

    pub const KEY_ENV: &str = "SB_DISTILLERS_TEST_CLASSIFY_KEY";
    static KEY: Once = Once::new();

    /// Set once per process, under Rust's env lock; every HTTP test shares
    /// the one name so no test ever unsets it under another.
    pub fn ensure_key() {
        KEY.call_once(|| unsafe { std::env::set_var(KEY_ENV, "test-key") });
    }

    pub fn serve<F>(respond: F) -> String
    where
        F: Fn(&serde_json::Value) -> Option<(u16, String)> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let body = read_request(&mut stream);
                match respond(&body) {
                    Some((status, body)) => {
                        let response = format!(
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    None => std::thread::sleep(Duration::from_secs(5)),
                }
            }
        });
        format!("http://127.0.0.1:{port}/v1/classify")
    }

    fn read_request(stream: &mut TcpStream) -> serde_json::Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut chunk).expect("read headers");
            assert!(n > 0, "client closed before the headers ended");
            buf.extend_from_slice(&chunk[..n]);
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
        let content_length: usize = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map(|v| v.trim().parse().expect("content-length"))
            .unwrap_or(0);
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = stream.read(&mut chunk).expect("read body");
            assert!(n > 0, "client closed before the body ended");
            body.extend_from_slice(&chunk[..n]);
        }
        serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
    }
}

fn dev_against(endpoint: String, timeout_secs: u64) -> ClassifierDev {
    dev_against_canon(endpoint, timeout_secs, canon())
}

fn dev_against_canon(endpoint: String, timeout_secs: u64, c: CanonicalSet) -> ClassifierDev {
    http::ensure_key();
    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::ClassifierDev,
        api_key_env: http::KEY_ENV.to_string(),
        endpoint,
        timeout_secs,
        ..TagsClassifierConfig::default()
    };
    ClassifierDev::new(c, mapping(), &cfg)
}

/// One `{"scores": {...}}` per input, every input scoring `rust`.
fn results_for(body: &serde_json::Value, score: f32) -> String {
    let n = body["inputs"].as_array().map_or(0, Vec::len);
    let results: Vec<serde_json::Value> = (0..n)
        .map(|_| serde_json::json!({ "scores": { "rust": score } }))
        .collect();
    serde_json::json!({ "results": results }).to_string()
}

#[test]
fn classifier_dev_401_is_an_error() {
    let endpoint = http::serve(|_| Some((401, r#"{"error":"unauthorized"}"#.to_string())));
    let err = dev_against(endpoint, 5).classify(&input(&[])).expect_err("401 must not classify");
    assert!(format!("{err:#}").contains("401"), "{err:#}");
}

#[test]
fn classifier_dev_429_is_an_error() {
    let endpoint = http::serve(|_| Some((429, r#"{"error":"rate limited"}"#.to_string())));
    let err = dev_against(endpoint, 5).classify(&input(&[])).expect_err("429 must not classify");
    assert!(format!("{err:#}").contains("429"), "{err:#}");
}

#[test]
fn classifier_dev_malformed_json_is_an_error() {
    let endpoint = http::serve(|_| Some((200, "{not json".to_string())));
    let err = dev_against(endpoint, 5)
        .classify(&input(&[]))
        .expect_err("a 200 with a body that does not parse must not classify");
    assert!(format!("{err:#}").contains("parse"), "{err:#}");
}

#[test]
fn classifier_dev_result_count_mismatch_is_an_error() {
    let endpoint = http::serve(|_| Some((200, r#"{"results":[]}"#.to_string())));
    let err = dev_against(endpoint, 5)
        .classify(&input(&[]))
        .expect_err("zero results for one input must not classify");
    assert!(format!("{err:#}").contains("0 results for 1 inputs"), "{err:#}");
}

/// A 200 whose one result carries no score at all. The API scores the top
/// `max_labels` labels for every input regardless of magnitude (Phase 0
/// recorded scores down to 0.71 with no floor), so this is a protocol
/// violation and an `Err`, not a `Low`: an `Ok(Low)` would hold the note or
/// publish it untagged while the receipt read clean, and the fallback path
/// (`degraded=true`) only runs on `Err`.
#[test]
fn classifier_dev_a_result_without_scores_is_an_error() {
    let endpoint = http::serve(|_| Some((200, r#"{"results":[{}]}"#.to_string())));
    let err = dev_against(endpoint, 5)
        .classify(&input(&[]))
        .expect_err("a result with no scores must not classify");
    assert!(format!("{err:#}").contains("scores"), "{err:#}");

    let endpoint = http::serve(|_| Some((200, r#"{"results":[{"scores":{}}]}"#.to_string())));
    let err = dev_against(endpoint, 5)
        .classify(&input(&[]))
        .expect_err("an empty scores map must not classify");
    assert!(format!("{err:#}").contains("no scores"), "{err:#}");
}

#[test]
fn classifier_dev_times_out_against_a_silent_server() {
    let endpoint = http::serve(|_| None);
    let started = std::time::Instant::now();
    let err = dev_against(endpoint, 1)
        .classify(&input(&[]))
        .expect_err("a server that never answers must not classify");
    assert!(started.elapsed() < Duration::from_secs(4), "timeout did not bind: {:?}", started.elapsed());
    assert!(format!("{err:#}").contains("request failed"), "{err:#}");
}

/// Design doc Testing Strategy: "one failed shard". A vocabulary over the
/// label ceiling takes two calls; the second is answered 500, and the input
/// must come back `Err` rather than ranked against half the vocabulary.
#[test]
fn classifier_dev_one_failed_shard_fails_the_input() {
    let all: HashSet<String> = (0..120).map(|i| format!("label-{i:03}")).collect();
    let c = CanonicalSet {
        all,
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 8,
    };
    assert_eq!(shard_labels(&{
        let mut v: Vec<String> = c.all.iter().cloned().collect();
        v.sort();
        v
    })
    .len(), 2);

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let endpoint = http::serve(move |body| {
        let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            Some((200, results_for(body, 0.95)))
        } else {
            Some((500, r#"{"error":"shard exploded"}"#.to_string()))
        }
    });
    let err = dev_against_canon(endpoint, 5, c)
        .classify(&input(&[]))
        .expect_err("a failed shard must fail the input");
    assert!(format!("{err:#}").contains("500"), "{err:#}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "both shards must have been attempted");
}

/// Implementation audit fold, A10: `classify_batch` POSTed the whole slice.
/// 1,001 inputs must go out as two calls of 1,000 and 1, and the results must
/// come back in input order across the chunk boundary.
#[test]
fn classify_batch_chunks_inputs_at_the_documented_ceiling() {
    let sizes = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
    let record = Arc::clone(&sizes);
    let endpoint = http::serve(move |body| {
        let inputs = body["inputs"].as_array().expect("inputs");
        record.lock().expect("lock").push(inputs.len());
        // Echo each input's parity back as a different tag so the caller can
        // check order, not just count.
        let results: Vec<serde_json::Value> = inputs
            .iter()
            .map(|text| {
                let n: usize = text
                    .as_str()
                    .expect("text")
                    .rsplit('n')
                    .next()
                    .expect("number")
                    .trim()
                    .parse()
                    .expect("parse number");
                let tag = if n % 2 == 0 { "rust" } else { "cli" };
                serde_json::json!({ "scores": { tag: 0.95 } })
            })
            .collect();
        Some((200, serde_json::json!({ "results": results }).to_string()))
    });

    let texts: Vec<String> = (0..MAX_INPUTS_PER_CALL + 1).map(|i| format!("n{i}")).collect();
    let inputs: Vec<TagInput> = texts
        .iter()
        .map(|t| TagInput {
            title: "T",
            text: t,
            candidates: &[],
        })
        .collect();
    let outputs = dev_against(endpoint, 10).classify_batch(&inputs);

    assert_eq!(outputs.len(), inputs.len());
    for (i, out) in outputs.iter().enumerate() {
        let out = out.as_ref().unwrap_or_else(|e| panic!("input {i} failed: {e:#}"));
        let want = if i % 2 == 0 { "rust" } else { "cli" };
        assert_eq!(out.tags, vec![want.to_string()], "input {i} came back out of order");
    }
    assert_eq!(
        *sizes.lock().expect("lock"),
        vec![MAX_INPUTS_PER_CALL, 1],
        "the batch must be chunked at the ceiling"
    );
}
