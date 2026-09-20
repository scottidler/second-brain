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
    let got = apply_retag(&previous, &fresh, &protect_list());
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
    let got = apply_retag(&previous, &fresh, &protect_list());
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
    assert_eq!(apply_retag(&previous, &empty, &protect_list()), previous);

    let low = TagOutput {
        tags: vec!["ai".to_string()],
        scores: None,
        confidence: Confidence::Low,
        method: TagMethod::ClassifierDev,
    };
    assert_eq!(apply_retag(&previous, &low, &protect_list()), previous);
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
