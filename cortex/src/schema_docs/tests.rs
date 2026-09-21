use super::*;

/// Directory holding one checked-in snapshot per generated doc. Same pattern
/// as `cortex/src/sweep/fixtures/cold-notes-expected.md`.
const FIXTURE_DIR: &str = "src/schema_docs/fixtures";

/// Fixed timestamp so the snapshots are reproducible. 2026-09-05T00:00:00Z.
fn fixed_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(1_788_566_400, 0).expect("fixed now")
}

fn fixture_path(filename: &str) -> String {
    format!("{FIXTURE_DIR}/{filename}")
}

/// Byte-exact equality between each rendered doc and its checked-in fixture.
/// After an intentional format or description change, run
/// `cargo test --package=cortex regenerate_schema_docs_snapshots -- --ignored`
/// and review the diff before re-committing.
#[test]
fn schema_docs_match_snapshot_fixtures() {
    let stamp = stamp(fixed_now());
    for spec in SPECS {
        let rendered = render_doc(spec, &stamp);
        let path = fixture_path(spec.filename);
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "missing snapshot fixture at {path}: {e}. Regenerate with \
                 `cargo test --package=cortex regenerate_schema_docs_snapshots -- --ignored`."
            )
        });
        assert_eq!(
            rendered, expected,
            "{} snapshot drift; regenerate with --ignored after reviewing the diff",
            spec.filename
        );
    }
}

/// Regenerate the snapshot fixtures. Run with
/// `cargo test --package=cortex regenerate_schema_docs_snapshots -- --ignored`
/// after an intentional change, then inspect the diff and re-commit.
#[test]
#[ignore = "writes checked-in fixtures; opt in via --ignored"]
fn regenerate_schema_docs_snapshots() {
    let stamp = stamp(fixed_now());
    std::fs::create_dir_all(FIXTURE_DIR).expect("create fixture dir");
    for spec in SPECS {
        let rendered = render_doc(spec, &stamp);
        let path = fixture_path(spec.filename);
        std::fs::write(&path, &rendered).expect("write snapshot fixture");
        eprintln!("wrote snapshot fixture to {path}");
    }
}

/// Every enum variant reaches the rendered table. This is the drift the whole
/// phase exists to kill: the hand-written `type-values.md` listed 15 of
/// `NoteType`'s 25 variants.
#[test]
fn every_enum_variant_appears_in_its_table() {
    let stamp = stamp(fixed_now());
    let rendered: Vec<(&str, String)> = SPECS
        .iter()
        .map(|spec| (spec.filename, render_doc(spec, &stamp)))
        .collect();

    let find = |name: &str| -> &String {
        &rendered
            .iter()
            .find(|(f, _)| *f == name)
            .unwrap_or_else(|| panic!("no rendered {name}"))
            .1
    };

    for t in vault::schema::NoteType::all() {
        let row = format!("| {} | {} |", t.as_str(), t.description());
        assert!(find("type-values.md").contains(&row), "missing type row: {row}");
    }
    for o in vault::schema::Origin::all() {
        let row = format!("| {} | {} |", o.as_str(), o.description());
        assert!(find("origin-values.md").contains(&row), "missing origin row: {row}");
    }
    for s in vault::schema::Status::all() {
        let row = format!("| {} | {} |", s.as_str(), s.description());
        assert!(find("status-values.md").contains(&row), "missing status row: {row}");
    }
}

/// The generated frontmatter carries every key the design doc names, and the
/// do-not-edit line sits in the body.
#[test]
fn generated_frontmatter_carries_the_required_keys() {
    let out = render_doc(&SPECS[0], "2026-09-05T00:00:00Z");
    assert!(out.starts_with("---\n"), "frontmatter present");
    for key in [
        "type: system",
        "origin: generated",
        "generated-at: 2026-09-05T00:00:00Z",
        "generator: sb cortex schema",
        "pinned: true",
        "date: 2026-09-05",
    ] {
        assert!(out.contains(key), "missing frontmatter key: {key}");
    }
    assert!(out.contains(DO_NOT_EDIT), "do-not-edit line present");
}

/// The stale naming rule ("no hyphens") is gone; several canonical keys
/// elsewhere in the vault are hyphenated, and the enums are the real
/// constraint.
#[test]
fn rules_blocks_drop_the_stale_no_hyphens_rule() {
    let stamp = stamp(fixed_now());
    for spec in SPECS {
        let out = render_doc(spec, &stamp);
        assert!(
            !out.contains("No hyphens"),
            "{} still carries the stale rule",
            spec.filename
        );
    }
}

/// A hand-written file (no `generated-at`) reports `Drifted` under `--check`,
/// and reports `Written` under `--render`. The written bytes then compare
/// `Unchanged` on the next pass, and a second `--render` leaves the file byte
/// identical (so `generated-at` does not churn).
#[test]
fn render_all_drifts_then_writes_then_settles() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join(SCHEMAS_DIR);
    std::fs::create_dir_all(&dir).expect("create schemas dir");
    for spec in SPECS {
        std::fs::write(dir.join(spec.filename), "---\ntitle: hand written\n---\n\nold\n").expect("seed");
    }
    std::fs::write(dir.join(TAG_VALUES_FILENAME), "---\ntitle: hand written\n---\n\nold\n").expect("seed");
    let tags = vec!["rust".to_string(), "tech".to_string()];

    let check = render_all_at(tmp.path(), false, fixed_now(), &tags).expect("check");
    assert!(check.drifted(), "hand-written files drift");
    assert_eq!(check.drifted_paths().len(), SPECS.len() + 1);
    assert!(
        check.files.iter().all(|f| f.outcome == Outcome::Drifted),
        "check writes nothing"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("type-values.md")).expect("read"),
        "---\ntitle: hand written\n---\n\nold\n",
        "--check must not touch the file"
    );

    let rendered = render_all_at(tmp.path(), true, fixed_now(), &tags).expect("render");
    assert!(rendered.files.iter().all(|f| f.outcome == Outcome::Written));
    assert!(!rendered.drifted());

    let after = render_all_at(tmp.path(), false, fixed_now(), &tags).expect("recheck");
    assert!(!after.drifted(), "rendered files no longer drift");
    assert!(after.files.iter().all(|f| f.outcome == Outcome::Unchanged));

    // A later run with a different clock must not rewrite an unchanged file.
    let before_bytes = std::fs::read_to_string(dir.join("type-values.md")).expect("read");
    let later = chrono::DateTime::<chrono::Utc>::from_timestamp(1_800_000_000, 0).expect("later");
    let again = render_all_at(tmp.path(), true, later, &tags).expect("render again");
    assert!(again.files.iter().all(|f| f.outcome == Outcome::Unchanged));
    assert_eq!(
        std::fs::read_to_string(dir.join("type-values.md")).expect("read"),
        before_bytes,
        "generated-at must not churn on an unchanged render"
    );
}

/// A missing file is drift, not an error, and `--render` creates it (including
/// the directory).
#[test]
fn render_all_creates_missing_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(SCHEMAS_DIR)).expect("create schemas dir");
    let tags = vec!["rust".to_string()];

    let check = render_all_at(tmp.path(), false, fixed_now(), &tags).expect("check");
    assert!(check.drifted(), "absent files count as drift");

    let rendered = render_all_at(tmp.path(), true, fixed_now(), &tags).expect("render");
    assert!(rendered.files.iter().all(|f| f.outcome == Outcome::Written));
    for spec in SPECS {
        assert!(
            tmp.path().join(SCHEMAS_DIR).join(spec.filename).is_file(),
            "{} written",
            spec.filename
        );
    }
    assert!(
        tmp.path().join(SCHEMAS_DIR).join(TAG_VALUES_FILENAME).is_file(),
        "tag-values.md written"
    );
}

/// P9 success criterion: `system/schemas/tag-values.md` exists after
/// `--render` and lists every canonical tag; a vocabulary change is drift.
#[test]
fn tag_values_doc_lists_every_canonical_tag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(SCHEMAS_DIR)).expect("create schemas dir");
    let tags = vec!["ai".to_string(), "rust".to_string(), "tech".to_string()];

    let rendered = render_all_at(tmp.path(), true, fixed_now(), &tags).expect("render");
    assert!(
        rendered
            .files
            .iter()
            .any(|f| f.path == format!("{SCHEMAS_DIR}/{TAG_VALUES_FILENAME}") && f.outcome == Outcome::Written)
    );

    let content = std::fs::read_to_string(tmp.path().join(SCHEMAS_DIR).join(TAG_VALUES_FILENAME)).expect("read");
    for tag in &tags {
        assert!(
            content.contains(&format!("| {tag} |")),
            "missing tag row: {tag}\n{content}"
        );
    }
    assert!(content.contains("## Values (3)"), "{content}");
    assert!(content.contains(DO_NOT_EDIT));

    // Unchanged on a second render against the same vocabulary.
    let again = render_all_at(tmp.path(), true, fixed_now(), &tags).expect("render again");
    assert!(
        again
            .files
            .iter()
            .any(|f| f.path == format!("{SCHEMAS_DIR}/{TAG_VALUES_FILENAME}") && f.outcome == Outcome::Unchanged)
    );

    // A vocabulary change is drift.
    let grown = vec![
        "ai".to_string(),
        "rust".to_string(),
        "tech".to_string(),
        "security".to_string(),
    ];
    let check = render_all_at(tmp.path(), false, fixed_now(), &grown).expect("check");
    assert!(
        check
            .files
            .iter()
            .any(|f| f.path == format!("{SCHEMAS_DIR}/{TAG_VALUES_FILENAME}") && f.outcome == Outcome::Drifted)
    );
}

/// A one-byte body edit to a generated file is caught even though its
/// `generated-at` is intact: the comparison neutralises only that field.
#[test]
fn a_body_edit_to_a_generated_file_is_drift() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join(SCHEMAS_DIR)).expect("create schemas dir");
    render_all_at(tmp.path(), true, fixed_now(), &[]).expect("render");

    let path = tmp.path().join(SCHEMAS_DIR).join("status-values.md");
    let edited = std::fs::read_to_string(&path)
        .expect("read")
        .replace("High value, reference often", "High value, reference oftenn");
    std::fs::write(&path, edited).expect("write edit");

    let check = render_all_at(tmp.path(), false, fixed_now(), &[]).expect("check");
    assert_eq!(check.drifted_paths(), vec!["system/schemas/status-values.md"]);
}

#[test]
fn disk_generated_at_only_reads_the_frontmatter_block() {
    assert_eq!(
        disk_generated_at("---\ngenerated-at: 2026-01-01T00:00:00Z\n---\n\nbody\n").as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(
        disk_generated_at("---\ntitle: x\n---\n\ngenerated-at: 2026-01-01T00:00:00Z\n"),
        None,
        "a body mention must not spoof the comparison"
    );
    assert_eq!(disk_generated_at("no frontmatter at all\n"), None);
}

#[test]
fn doc_paths_names_the_four_generated_files() {
    let names: Vec<String> = doc_paths().iter().map(|p| p.to_string_lossy().into_owned()).collect();
    assert_eq!(
        names,
        vec![
            "system/schemas/type-values.md",
            "system/schemas/origin-values.md",
            "system/schemas/status-values.md",
            "system/schemas/tag-values.md",
        ]
    );
    assert!(
        !names.iter().any(|n| n.contains("frontmatter")),
        "frontmatter.md stays hand-written"
    );
}

/// B6: the generated frontmatter spells `tags` in block form, the form the
/// `tags.format` lint requires, for every generated doc including the
/// data-driven tag doc.
#[test]
fn generated_frontmatter_writes_tags_in_block_form() {
    let stamp = stamp(fixed_now());
    let mut rendered: Vec<String> = SPECS.iter().map(|spec| render_doc(spec, &stamp)).collect();
    rendered.push(render_tag_values_doc(&["rust".to_string()], &stamp));
    for doc in rendered {
        let (frontmatter, _) = doc
            .strip_prefix("---\n")
            .and_then(|b| b.split_once("\n---\n"))
            .expect("frontmatter block");
        assert!(
            frontmatter.contains("\ntags:\n  - obsidian"),
            "tags not in block form:\n{frontmatter}"
        );
        assert!(
            !frontmatter.contains("tags: ["),
            "inline tags list survived:\n{frontmatter}"
        );
    }
}
