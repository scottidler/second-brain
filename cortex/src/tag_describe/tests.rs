#![allow(clippy::unwrap_used)]

use super::*;
use std::cell::RefCell;
use vault::frontmatter::Frontmatter;

const VOCAB: &str = "\
max-per-note: 8
max-canonical: 300
no-classifier-tags:
  - work
tags:
  tech:
    - rust
    - python: \"the Python language\"
  life:
    - work
    - football
";

fn vocab_file(dir: &Path) -> PathBuf {
    let path = dir.join("canonical-tags.yml");
    std::fs::write(&path, VOCAB).unwrap();
    path
}

fn note(path: &str, date: &str, tags: &[&str], body: &str) -> Note {
    Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            title: Some(format!("title of {path}")),
            date: Some(date.to_string()),
            tags: Some(tags.iter().map(|t| t.to_string()).collect()),
            ..Default::default()
        },
        body: body.to_string(),
        raw: String::new(),
    }
}

fn opts(tags: &[&str]) -> DescribeOpts {
    DescribeOpts {
        tags: tags.iter().map(|t| t.to_string()).collect(),
        samples: DEFAULT_SAMPLES,
        ..Default::default()
    }
}

/// A fake fabric runner: records every request and answers from `reply`.
struct FakeDrafter {
    reply: String,
    requests: RefCell<Vec<DescribeRequest>>,
}

impl FakeDrafter {
    fn answering(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            requests: RefCell::new(Vec::new()),
        }
    }
}

impl TagDrafter for FakeDrafter {
    fn draft(&self, request: &DescribeRequest) -> Result<String> {
        self.requests.borrow_mut().push(request.clone());
        parse_description(&request.tag, &self.reply)
    }
}

const RUST_REPLY: &str = r#"{"description": "the Rust programming language: cargo, crates, borrow checker, async tokio; not rust corrosion"}"#;

// ---------------------------------------------------------------------------
// parse_description
// ---------------------------------------------------------------------------

#[test]
fn parse_description_reads_a_fenced_json_answer() {
    let out = parse_description("rust", "```json\n{\"description\": \"  the Rust language  \"}\n```").unwrap();
    assert_eq!(out, "the Rust language");
}

#[test]
fn parse_description_refuses_output_with_no_object() {
    let err = parse_description("rust", "I could not decide.").unwrap_err();
    assert!(format!("{err:#}").contains("no JSON object"), "{err:#}");
}

#[test]
fn parse_description_refuses_a_description_over_the_cap() {
    let long = "x".repeat(vault::canonical::MAX_DESCRIPTION_CHARS + 1);
    let err = parse_description("rust", &format!("{{\"description\": \"{long}\"}}")).unwrap_err();
    assert!(format!("{err:#}").contains("200-char cap"), "{err:#}");
}

#[test]
fn parse_description_refuses_an_embedded_double_quote() {
    let err = parse_description("rust", r#"{"description": "the \"Rust\" language"}"#).unwrap_err();
    assert!(format!("{err:#}").contains("contains `\"`"), "{err:#}");
}

// ---------------------------------------------------------------------------
// load_exclude_list, select_samples
// ---------------------------------------------------------------------------

#[test]
fn exclude_list_reads_one_vault_path_per_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gold-exclude.txt");
    std::fs::write(&path, "notes/a.md\n\n./notes/b.md\n  notes/c.md  \n").unwrap();
    let got = load_exclude_list(&path).unwrap();
    let want: HashSet<String> = ["notes/a.md", "notes/b.md", "notes/c.md"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, want);
}

#[test]
fn exclude_list_refuses_an_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gold-exclude.txt");
    std::fs::write(&path, "notes/a.md\n/home/x/vault/notes/b.md\n").unwrap();
    let err = load_exclude_list(&path).unwrap_err();
    assert!(format!("{err:#}").contains("line 2"), "{err:#}");
}

#[test]
fn samples_are_tagged_notes_newest_first_minus_the_excluded() {
    let notes = vec![
        note("notes/old.md", "2026-01-01", &["rust"], ""),
        note("notes/gold.md", "2026-09-01", &["rust"], ""),
        note("notes/new.md", "2026-08-01", &["rust"], ""),
        note("notes/python.md", "2026-09-20", &["python"], ""),
        note("notes/mid.md", "2026-05-01", &["cli", "rust"], ""),
    ];
    let exclude: HashSet<String> = ["notes/gold.md".to_string()].into();
    let picked: Vec<String> = select_samples(&notes, "rust", &exclude, 2)
        .iter()
        .map(|n| n.path.to_string_lossy().to_string())
        .collect();
    assert_eq!(picked, ["notes/new.md", "notes/mid.md"]);
}

// ---------------------------------------------------------------------------
// draft_descriptions (dry run)
// ---------------------------------------------------------------------------

#[test]
fn a_dry_run_drafts_caches_the_samples_and_leaves_the_vocabulary_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    let notes = vec![
        note(
            "notes/a.md",
            "2026-09-02",
            &["rust"],
            "## Summary\n\nborrow checker tips\n\n## Claims\n- x",
        ),
        note("notes/gold.md", "2026-09-03", &["rust"], "gold body"),
        note("notes/b.md", "2026-09-01", &["rust"], "tokio body"),
    ];
    let mut o = opts(&["rust"]);
    o.exclude = ["notes/gold.md".to_string()].into();
    let drafter = FakeDrafter::answering(RUST_REPLY);

    let report = draft_descriptions(&canonical, &cache, &notes, &o, &drafter).unwrap();

    assert!(!report.applied);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        std::fs::read_to_string(&canonical).unwrap(),
        VOCAB,
        "dry run wrote the vocabulary"
    );

    let requests = drafter.requests.borrow();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.group, "tech");
    assert_eq!(
        request.siblings,
        vec![TagEntry::described("python", "the Python language").unwrap()]
    );
    let texts: Vec<&str> = request.samples.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "title of notes/a.md\n\nborrow checker tips",
            "title of notes/b.md\n\ntokio body"
        ]
    );
    assert!(
        request.input().contains("- python: \"the Python language\""),
        "{}",
        request.input()
    );

    let cached = CachedDraft::load(&cache, "rust").unwrap();
    assert_eq!(cached.source, DraftSource::Fabric);
    assert_eq!(cached.group, "tech");
    assert_eq!(cached.samples, ["notes/a.md", "notes/b.md"]);
    assert!(cached.description.starts_with("the Rust programming language"));
    assert_eq!(report.described[0].cache_path, CachedDraft::path(&cache, "rust"));
}

#[test]
fn a_protect_list_tag_refuses_a_draft_naming_the_protect_list() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let drafter = FakeDrafter::answering(RUST_REPLY);

    let err = draft_descriptions(&canonical, &dir.path().join("cache"), &[], &opts(&["work"]), &drafter).unwrap_err();

    let msg = format!("{err:#}");
    assert!(
        msg.contains("protect list") && msg.contains("no-classifier-tags"),
        "{msg}"
    );
    assert!(drafter.requests.borrow().is_empty(), "drafted a protected tag");
}

#[test]
fn allow_protected_lets_a_protect_list_tag_be_drafted() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let mut o = opts(&["work"]);
    o.allow_protected = true;
    let drafter = FakeDrafter::answering(r#"{"description": "paid employment at Tatari: on-call, sprint work"}"#);

    let report = draft_descriptions(&canonical, &dir.path().join("cache"), &[], &o, &drafter).unwrap();

    assert_eq!(report.described.len(), 1);
    assert_eq!(drafter.requests.borrow()[0].siblings, vec![TagEntry::bare("football")]);
}

#[test]
fn set_wording_needs_no_llm_call_and_no_allow_protected() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    let o = DescribeOpts {
        set: vec![("work".to_string(), "my job at Tatari".to_string())],
        samples: DEFAULT_SAMPLES,
        ..Default::default()
    };
    let drafter = FakeDrafter::answering("unused");

    let report = draft_descriptions(&canonical, &cache, &[], &o, &drafter).unwrap();

    assert!(drafter.requests.borrow().is_empty());
    let cached = CachedDraft::load(&cache, "work").unwrap();
    assert_eq!(cached.source, DraftSource::Set);
    assert!(cached.samples.is_empty());
    assert_eq!(report.described[0].draft.description, "my job at Tatari");
}

#[test]
fn an_unknown_tag_fails_the_run_before_any_draft() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let drafter = FakeDrafter::answering(RUST_REPLY);

    let err = draft_descriptions(
        &canonical,
        &dir.path().join("cache"),
        &[],
        &opts(&["rust", "nope"]),
        &drafter,
    )
    .unwrap_err();

    assert!(format!("{err:#}").contains("\"nope\" is not in"), "{err:#}");
    assert!(drafter.requests.borrow().is_empty());
}

#[test]
fn a_bad_draft_fails_its_tag_and_the_rest_still_draft() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");

    struct PerTag;
    impl TagDrafter for PerTag {
        fn draft(&self, request: &DescribeRequest) -> Result<String> {
            match request.tag.as_str() {
                "rust" => parse_description("rust", "no json here"),
                _ => parse_description(&request.tag, r#"{"description": "american football: plays, schemes"}"#),
            }
        }
    }

    let report = draft_descriptions(&canonical, &cache, &[], &opts(&["rust", "football"]), &PerTag).unwrap();

    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, "rust");
    assert_eq!(report.described.len(), 1);
    assert!(!CachedDraft::path(&cache, "rust").exists(), "a failed draft was cached");
    assert!(CachedDraft::path(&cache, "football").exists());
}

// ---------------------------------------------------------------------------
// apply_descriptions
// ---------------------------------------------------------------------------

#[test]
fn apply_writes_the_cached_draft_and_changes_no_other_line() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    draft_descriptions(
        &canonical,
        &cache,
        &[],
        &opts(&["rust"]),
        &FakeDrafter::answering(RUST_REPLY),
    )
    .unwrap();
    let cached = CachedDraft::load(&cache, "rust").unwrap();

    let report = apply_descriptions(&canonical, &cache, &opts(&["rust"])).unwrap();

    assert!(report.applied);
    let after = std::fs::read_to_string(&canonical).unwrap();
    let want = VOCAB.replace("    - rust\n", &format!("    - rust: \"{}\"\n", cached.description));
    assert_eq!(after, want);
    let reparsed: CanonicalTagsFile = serde_yaml::from_str(&after).unwrap();
    assert_eq!(
        reparsed.canonical_set().descriptions.get("rust"),
        Some(&cached.description)
    );
}

#[test]
fn apply_with_no_cached_draft_refuses_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    let o = DescribeOpts {
        tags: vec!["rust".to_string()],
        set: vec![("football".to_string(), "american football".to_string())],
        ..Default::default()
    };

    let err = apply_descriptions(&canonical, &cache, &o).unwrap_err();

    assert!(format!("{err:#}").contains("no cached draft for \"rust\""), "{err:#}");
    assert_eq!(
        std::fs::read_to_string(&canonical).unwrap(),
        VOCAB,
        "a partial apply wrote the set tag"
    );
}

#[test]
fn apply_refuses_a_cached_draft_for_a_protect_list_tag_without_allow_protected() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    let mut drafting = opts(&["work"]);
    drafting.allow_protected = true;
    draft_descriptions(
        &canonical,
        &cache,
        &[],
        &drafting,
        &FakeDrafter::answering(r#"{"description": "paid employment"}"#),
    )
    .unwrap();

    let err = apply_descriptions(&canonical, &cache, &opts(&["work"])).unwrap_err();
    assert!(format!("{err:#}").contains("protect list"), "{err:#}");
    assert_eq!(std::fs::read_to_string(&canonical).unwrap(), VOCAB);

    apply_descriptions(&canonical, &cache, &drafting).unwrap();
    assert!(
        std::fs::read_to_string(&canonical)
            .unwrap()
            .contains("    - work: \"paid employment\"\n")
    );
}

#[test]
fn apply_set_writes_the_given_wording_and_caches_it() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    let o = DescribeOpts {
        set: vec![("python".to_string(), "Python: pip, uv, pytest; not snakes".to_string())],
        ..Default::default()
    };

    apply_descriptions(&canonical, &cache, &o).unwrap();

    let after = std::fs::read_to_string(&canonical).unwrap();
    assert_eq!(
        after,
        VOCAB.replace(
            "    - python: \"the Python language\"\n",
            "    - python: \"Python: pip, uv, pytest; not snakes\"\n"
        )
    );
    assert_eq!(CachedDraft::load(&cache, "python").unwrap().source, DraftSource::Set);
}

#[test]
fn a_cached_draft_for_another_tag_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let canonical = vocab_file(dir.path());
    let cache = dir.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        CachedDraft::path(&cache, "rust"),
        r#"{"tag": "python", "group": "tech", "description": "x", "source": "fabric", "samples": []}"#,
    )
    .unwrap();

    let err = apply_descriptions(&canonical, &cache, &opts(&["rust"])).unwrap_err();
    assert!(format!("{err:#}").contains("not \"rust\""), "{err:#}");
}

// ---------------------------------------------------------------------------
// FabricDrafter through a fake fabric binary
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn the_fabric_drafter_sends_the_request_and_reads_the_answer() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("stdin.txt");
    let bin = dir.path().join("fabric");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\ncat > {}\necho '{}'\n",
            seen.display(),
            r#"{"description": "the Rust language: cargo, crates"}"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fabric = FabricConfig {
        binary: bin.to_string_lossy().to_string(),
        api_key: String::new(),
        ..Default::default()
    };
    let drafter = FabricDrafter {
        fabric: &fabric,
        pattern: DESCRIBE_TAG_PATTERN,
        timeout_secs: 10,
    };
    let request = DescribeRequest {
        tag: "rust".to_string(),
        group: "tech".to_string(),
        siblings: vec![TagEntry::bare("python")],
        samples: vec![Sample {
            path: "notes/a.md".to_string(),
            text: "A title\n\nA summary".to_string(),
        }],
    };

    let description = drafter.draft(&request).unwrap();

    assert_eq!(description, "the Rust language: cargo, crates");
    let sent = std::fs::read_to_string(&seen).unwrap();
    assert!(sent.starts_with("TAG: rust\nGROUP: tech\n"), "{sent}");
    assert!(
        sent.contains("- python\n") && sent.contains("### 1\nA title\n\nA summary"),
        "{sent}"
    );
}
