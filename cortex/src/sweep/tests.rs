use super::*;
use crate::testutil::NoteBuilder;

fn make_config(dir: &Path) -> SweepConfig {
    let canonical_path = dir.join("canonical-tags.yml");
    let mapping_path = dir.join("tag-mapping.yml");
    let proposals_path = dir.join("tag-proposals.yml");

    std::fs::write(
            &canonical_path,
            "max-per-note: 3\nmax-canonical: 300\ntags:\n  ai:\n    - ai\n    - claude\n    - llm\n  tech:\n    - rust\n    - python\n",
        )
        .expect("write canonical");
    std::fs::write(
        &mapping_path,
        "ai-agents: ai\nai-coding: ai\nclaudecodeai: null\nrustlang: rust\n",
    )
    .expect("write mapping");
    std::fs::write(&proposals_path, "proposals: []\n").expect("write proposals");

    SweepConfig {
        canonical_path,
        mapping_path,
        proposals_path,
        sweep_interval: "1h".to_string(),
        proposal_threshold: 2,
        staged_proposals: false,
        cold: crate::config::ColdConfig::default(),
    }
}

fn make_cold_note(path: &str, title: &str, date: &str) -> ColdNote {
    ColdNote {
        path: path.to_string(),
        title: title.to_string(),
        date: date.to_string(),
    }
}

/// Fixed input used by both the snapshot assertion and the
/// (ignored) regeneration test. Keeping the construction in a
/// shared helper guarantees the regen path and the comparison
/// path see identical input.
fn snapshot_fixture_input() -> (Vec<ColdNote>, ColdStats, u32, chrono::DateTime<chrono::Utc>) {
    let rows = vec![
        make_cold_note("notes/ai/old-paper.md", "Old Paper", "2025-08-12"),
        make_cold_note("notes/ai/forgotten.md", "Forgotten Thread", "2025-08-12"),
        make_cold_note("notes/diy/unused-jig.md", "Unused Jig", "2025-08-12"),
    ];
    let stats = ColdStats {
        scanned: 1_345,
        surfaced: 3,
        pinned_excluded: 7,
    };
    let now = chrono::DateTime::<chrono::Utc>::from_timestamp(1_747_569_600, 0).expect("fixed now");
    (rows, stats, 180, now)
}

const SNAPSHOT_FIXTURE_PATH: &str = "src/sweep/fixtures/cold-notes-expected.md";

/// Byte-exact equality between `render_cold_report_at` output and
/// the checked-in fixture. If this fails after an intentional format
/// change, run `cargo test -p cortex regenerate_cold_report_snapshot
/// -- --ignored` and review the diff before re-committing.
#[test]
fn cold_report_matches_snapshot_fixture() {
    let (rows, stats, days, now) = snapshot_fixture_input();
    let rendered = render_cold_report_at(&rows, &stats, days, now);
    let expected = std::fs::read_to_string(SNAPSHOT_FIXTURE_PATH).unwrap_or_else(|e| {
        panic!(
            "missing snapshot fixture at {SNAPSHOT_FIXTURE_PATH}: {e}. \
                 Regenerate with `cargo test -p cortex regenerate_cold_report_snapshot -- --ignored`."
        )
    });
    assert_eq!(
        rendered, expected,
        "cold-report snapshot drift; regenerate with --ignored after reviewing the diff",
    );
}

/// Regenerate the snapshot fixture. Run with `cargo test -p cortex
/// regenerate_cold_report_snapshot -- --ignored` after an
/// intentional format change, then inspect the diff and re-commit.
#[test]
#[ignore = "writes a checked-in fixture; opt in via --ignored"]
fn regenerate_cold_report_snapshot() {
    let (rows, stats, days, now) = snapshot_fixture_input();
    let rendered = render_cold_report_at(&rows, &stats, days, now);
    std::fs::write(SNAPSHOT_FIXTURE_PATH, &rendered).expect("write snapshot fixture");
    eprintln!("wrote snapshot fixture to {SNAPSHOT_FIXTURE_PATH}");
}

#[test]
fn render_cold_report_flat_list_includes_metadata() {
    let rows = vec![
        make_cold_note("notes/ai/a.md", "A Paper", "2025-08-12"),
        make_cold_note("notes/ai/b.md", "B Thing", "2025-08-12"),
        make_cold_note("notes/diy/c.md", "C Hack", "2025-08-12"),
    ];
    let stats = ColdStats {
        scanned: 100,
        surfaced: 3,
        pinned_excluded: 7,
    };
    let out = render_cold_report(&rows, &stats, 180);

    assert!(out.starts_with("---\n"), "frontmatter present");
    assert!(out.contains("older-than-days: 180"));
    assert!(out.contains("total-surfaced: 3"));
    assert!(out.contains("pinned-excluded: 7"));
    assert!(out.contains("pinned: true"), "report file marks itself pinned");
    // No grouping headers: every row is a flat checklist item.
    assert!(!out.contains("##"), "no group headers in the flat list");
    assert!(out.contains("- [ ] `notes/ai/a.md`"));
    assert!(out.contains("- [ ] `notes/ai/b.md`"));
    assert!(out.contains("- [ ] `notes/diy/c.md`"));
    assert!(out.contains("\"A Paper\""));
    assert!(out.contains("dated 2025-08-12"));
}

#[test]
fn render_cold_report_empty_writes_placeholder() {
    let stats = ColdStats {
        scanned: 100,
        surfaced: 0,
        pinned_excluded: 0,
    };
    let out = render_cold_report(&[], &stats, 180);
    assert!(out.contains("No cold notes at the current threshold."));
}

#[test]
fn render_cold_report_lists_a_single_row_with_no_group_header() {
    let rows = vec![make_cold_note("notes/loose.md", "Loose", "2025-08-12")];
    let stats = ColdStats {
        scanned: 1,
        surfaced: 1,
        pinned_excluded: 0,
    };
    let out = render_cold_report(&rows, &stats, 180);
    assert!(out.contains("- [ ] `notes/loose.md`"));
    assert!(!out.contains("##"), "no group header for a single row");
}

/// `test_daemon_cold_tick_fires` per the design doc. Goes through
/// `sweep::daemon_cold_tick` - the exact entry point the daemon's
/// `select!` arm invokes - so a regression in (a) the daemon ->
/// sweep wiring, (b) `config.oracle_db_path()` resolution, or (c)
/// the run_cold body would all break this test. We intentionally do
/// NOT drive `start_watching` in a spawned task: the tokio
/// `interval` firing is tokio's responsibility, and the surrounding
/// daemon orchestration (watcher, initial sweep, intel scheduling)
/// pulls in heavy machinery for a path that's already covered by
/// the surrounding unit tests. The chronology the design doc
/// specifies ("a few seconds") is collapsed to a single synchronous
/// invocation here.
///
/// Linux-only because we redirect `dirs::data_local_dir()` via
/// `XDG_DATA_HOME`. macOS resolves data_local_dir via system APIs
/// that ignore env vars (see the `dirs` crate platform notes).
#[cfg(target_os = "linux")]
#[serial_test::serial(xdg_data_home)]
#[test]
fn test_daemon_cold_tick_fires() {
    use std::path::PathBuf;
    use vault::frontmatter::Frontmatter;
    use vault::note::Note;
    use vault::search::SearchIndex;

    let xdg_tmp = tempfile::tempdir().expect("xdg tmpdir");
    let vault_tmp = tempfile::tempdir().expect("vault tmpdir");

    // Redirect `dirs::data_local_dir()` so config.oracle_db_path()
    // resolves under our tempdir instead of the real user store.
    // safety: behind serial_test::serial(xdg_data_home), no
    // concurrent test mutates XDG_DATA_HOME.
    let prior = std::env::var_os("XDG_DATA_HOME");
    // SAFETY: tests serialized by the `xdg_data_home` lock; no
    // concurrent reader exists, so the mutation is sound here.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", xdg_tmp.path());
    }

    let result = std::panic::catch_unwind(|| {
        // Resolve the DB path the same way production does.
        let config = crate::config::Config::default();
        let db_path = config.oracle_db_path();
        assert!(
            db_path.starts_with(xdg_tmp.path()),
            "XDG_DATA_HOME redirect did not take: db_path={}",
            db_path.display(),
        );
        std::fs::create_dir_all(db_path.parent().expect("db parent")).expect("mkdir db parent");

        // Pre-seed the DB with a cold note (no signals, old content date).
        let index = SearchIndex::open(&db_path).expect("open db");
        let fm = Frontmatter {
            title: Some("Stale Note".to_string()),
            date: Some("2020-01-01".to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            ..Frontmatter::default()
        };
        let note = Note {
            path: PathBuf::from("notes/ai/stale.md"),
            frontmatter: fm,
            body: "## Summary\n\nS.\n".to_string(),
            raw: String::new(),
        };
        index.index_one(&note, 1_000).expect("seed cold note");
        drop(index);

        // Invoke the same function the daemon's select! arm calls.
        let stats = daemon_cold_tick(vault_tmp.path(), &config).expect("daemon_cold_tick");
        assert_eq!(stats.surfaced, 1, "the cold note must surface");

        let report = vault_tmp.path().join("system").join("views").join("cold-notes.md");
        assert!(report.exists(), "report file must appear at {}", report.display());
        let body = std::fs::read_to_string(&report).expect("read report");
        assert!(body.contains("`notes/ai/stale.md`"), "report must list the cold note");
    });

    // SAFETY: same serialization guarantee as the set_var above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn cold_with_index_counts_pinned_excluded() {
    use std::path::PathBuf;
    use vault::frontmatter::Frontmatter;
    use vault::note::Note;
    use vault::search::SearchIndex;

    let index = SearchIndex::open_memory().expect("open");
    // Old, otherwise-cold, pinned: should be counted as
    // pinned_excluded, NOT surfaced.
    let fm_pinned = Frontmatter {
        title: Some("Pinned".to_string()),
        date: Some("2020-01-01".to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        pinned: Some(true),
        ..Frontmatter::default()
    };
    let pinned_note = Note {
        path: PathBuf::from("notes/ai/pinned.md"),
        frontmatter: fm_pinned,
        body: "## Summary\n\nP.\n".to_string(),
        raw: String::new(),
    };
    index.index_one(&pinned_note, 1_000).expect("index pinned");

    // Old, unpinned, cold: should surface.
    let fm_cold = Frontmatter {
        title: Some("Cold".to_string()),
        date: Some("2020-01-01".to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        ..Frontmatter::default()
    };
    let cold_note = Note {
        path: PathBuf::from("notes/ai/cold.md"),
        frontmatter: fm_cold,
        body: "## Summary\n\nC.\n".to_string(),
        raw: String::new(),
    };
    index.index_one(&cold_note, 1_000).expect("index cold");

    let vault_root = tempfile::tempdir().expect("tmpdir");
    let cold_config = crate::config::ColdConfig {
        older_than_days: 30,
        limit: 100,
    };
    let stats = cold_with_index(vault_root.path(), &index, &cold_config).expect("run_cold");

    assert_eq!(stats.scanned, 2, "two indexed notes");
    assert_eq!(stats.surfaced, 1, "only the unpinned one surfaces");
    assert_eq!(stats.pinned_excluded, 1, "the pinned one counts in the floor stat");

    let report_path = vault_root.path().join("system").join("views").join("cold-notes.md");
    let body = std::fs::read_to_string(&report_path).expect("read");
    assert!(body.contains("`notes/ai/cold.md`"));
    assert!(!body.contains("`notes/ai/pinned.md`"), "pinned must not surface");
    assert!(body.contains("pinned-excluded: 1"));
}

#[test]
fn cold_with_index_writes_report_atomically() {
    // The same fixture pattern the daemon test would otherwise need
    // (driving the daemon for a few seconds and asserting the file
    // appears). Going through `cold_with_index` directly skips
    // the tokio interval but exercises every other step the daemon
    // tick runs, so a regression in the SQL, render, or atomic
    // write will surface here.
    use std::path::PathBuf;
    use vault::frontmatter::Frontmatter;
    use vault::note::Note;
    use vault::search::SearchIndex;

    let index = SearchIndex::open_memory().expect("open");
    let fm_cold = Frontmatter {
        title: Some("Old Paper".to_string()),
        date: Some("2020-01-01".to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        ..Frontmatter::default()
    };
    let cold_note = Note {
        path: PathBuf::from("notes/ai/old.md"),
        frontmatter: fm_cold,
        body: "## Summary\n\nO.\n".to_string(),
        raw: String::new(),
    };
    index.index_one(&cold_note, 1_000).expect("index");

    let vault_root = tempfile::tempdir().expect("tmpdir");
    let cold_config = crate::config::ColdConfig {
        older_than_days: 30,
        limit: 100,
    };
    let stats = cold_with_index(vault_root.path(), &index, &cold_config).expect("run_cold");

    assert_eq!(stats.scanned, 1);
    assert_eq!(stats.surfaced, 1);
    assert_eq!(stats.pinned_excluded, 0);

    let report_path = vault_root.path().join("system").join("views").join("cold-notes.md");
    assert!(
        report_path.exists(),
        "report file should exist at {}",
        report_path.display()
    );
    let body = std::fs::read_to_string(&report_path).expect("read report");
    assert!(body.contains("`notes/ai/old.md`"));
    assert!(body.contains("\"Old Paper\""));
    // Atomic write: temp file should not survive the rename.
    let tmp_path = report_path.with_extension("md.tmp");
    assert!(!tmp_path.exists(), "temp file should not linger");
}

#[test]
fn render_cold_report_handles_missing_title() {
    let rows = vec![make_cold_note("notes/a.md", "", "2025-08-12")];
    let stats = ColdStats {
        scanned: 1,
        surfaced: 1,
        pinned_excluded: 0,
    };
    let out = render_cold_report(&rows, &stats, 180);
    assert!(out.contains("\"(untitled)\""));
}

#[test]
fn test_scan_proposals_finds_non_canonical() {
    // `scan_proposals` calls `crate::startup::validate_canonical_assets()`,
    // which resolves the REAL `XDG_CONFIG_HOME`-relative canonical-tags/
    // tag-mapping files (not this test's own `make_config` paths) - acquire
    // the suite-wide lock so this can't race `startup/tests.rs`'s env
    // mutation under parallel `cargo test`.
    let _lock = crate::testutil::lock_env();
    // Private XDG_CONFIG_HOME so `validate_canonical_assets` checks these
    // files rather than the developer's real ~/.config/sb/.
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());

    let notes = vec![
        NoteBuilder::new("notes/a.md").tags(&["unknown-tag", "ai"]).build(),
        NoteBuilder::new("notes/b.md").tags(&["unknown-tag", "rust"]).build(),
        NoteBuilder::new("notes/c.md").tags(&["other-tag", "python"]).build(),
    ];

    let proposals = scan_proposals(dir.path(), &notes, &config, &dir.path().join("no-staging"))
        .expect("scan")
        .proposals;
    // "unknown-tag" appears on 2 notes, meets threshold of 2
    assert!(proposals.iter().any(|p| p.tag == "unknown-tag"));
    // "other-tag" appears on 1 note, below threshold
    assert!(!proposals.iter().any(|p| p.tag == "other-tag"));
}

#[test]
fn test_scan_proposals_mapped_tags_not_proposed() {
    // See the lock comment on `test_scan_proposals_finds_non_canonical`.
    let _lock = crate::testutil::lock_env();
    // Private XDG_CONFIG_HOME so `validate_canonical_assets` checks these
    // files rather than the developer's real ~/.config/sb/.
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());

    let notes = vec![
        NoteBuilder::new("notes/a.md").tags(&["ai-agents", "rustlang"]).build(),
        NoteBuilder::new("notes/b.md").tags(&["ai-agents", "python"]).build(),
    ];

    let proposals = scan_proposals(dir.path(), &notes, &config, &dir.path().join("no-staging"))
        .expect("scan")
        .proposals;
    // ai-agents maps to "ai" in the mapping file, so it should NOT be proposed
    assert!(proposals.is_empty());
}

/// Design doc `2026-09-21-staged-tag-proposals.md`, Phase 1: a `null`-mapped
/// (human-rejected) tag must never be re-proposed. Before this guard,
/// `match_to_canonical` returns `vec![]` for a rejection exactly like it does
/// for "no match at all", so `scan_proposals` treated the two identically and
/// proposed the reject once it cleared the threshold.
#[test]
fn test_scan_proposals_rejected_tags_not_proposed() {
    // See the lock comment on `test_scan_proposals_finds_non_canonical`.
    let _lock = crate::testutil::lock_env();
    // Private XDG_CONFIG_HOME so `validate_canonical_assets` checks these
    // files rather than the developer's real ~/.config/sb/.
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());

    let notes = vec![
        // "claudecodeai" is mapped to `null` (explicit reject) in
        // `make_config`'s mapping fixture, and appears on 2 notes, which
        // clears the threshold of 2.
        NoteBuilder::new("notes/a.md").tags(&["claudecodeai", "rust"]).build(),
        NoteBuilder::new("notes/b.md").tags(&["claudecodeai", "python"]).build(),
    ];

    let proposals = scan_proposals(dir.path(), &notes, &config, &dir.path().join("no-staging"))
        .expect("scan")
        .proposals;
    assert!(proposals.is_empty());
}

/// Design doc `2026-07-05-cortex-daemon-oscillation-loop.md`, Phase 1: the
/// sweep arm's fingerprint may only include paths `rewrite_note_tags`
/// actually wrote, never every `new_tags != tags` diff (sweep.rs:174 in the
/// pre-fix code). This note's IN-MEMORY frontmatter carries a non-canonical
/// tag (so `new_tags != tags`), but its ON-DISK content has no frontmatter
/// block at all - simulating a note whose frontmatter vanished between scan
/// and this migrate call. `replace_tags_in_frontmatter` returns `None` for
/// that content, so no write ever lands.
#[test]
fn migrate_excludes_paths_rewrite_note_tags_could_not_write() {
    // `migrate` calls `crate::startup::validate_canonical_assets()`, which
    // resolves the REAL `XDG_CONFIG_HOME`-relative assets - see the lock
    // comment on `test_scan_proposals_finds_non_canonical`.
    let _lock = crate::testutil::lock_env();
    // Private XDG_CONFIG_HOME so `validate_canonical_assets` checks these
    // files rather than the developer's real ~/.config/sb/.
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("assets tmpdir");
    let config = make_config(dir.path());
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();

    let note_path = "no-frontmatter.md";
    let original = "Just body text, no frontmatter.\n";
    std::fs::write(vault_root.join(note_path), original).expect("write note");

    let notes = vec![NoteBuilder::new(note_path).tags(&["unknown-tag"]).build()];

    let modified = migrate(vault_root, &notes, &config, false).expect("migrate");
    assert!(
        modified.is_empty(),
        "migrate must not report a path whose write never landed: {modified:?}"
    );

    let content = std::fs::read_to_string(vault_root.join(note_path)).expect("read note");
    assert_eq!(
        content, original,
        "bytes must be unchanged when rewrite_note_tags could not write"
    );
}

/// Companion happy-path: when the frontmatter block IS present, the write
/// really lands and `migrate` reports it.
#[test]
fn migrate_includes_paths_actually_rewritten() {
    // See the lock comment on `migrate_excludes_paths_rewrite_note_tags_could_not_write`.
    let _lock = crate::testutil::lock_env();
    // Private XDG_CONFIG_HOME so `validate_canonical_assets` checks these
    // files rather than the developer's real ~/.config/sb/.
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("assets tmpdir");
    let config = make_config(dir.path());
    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();

    let note_path = "has-frontmatter.md";
    std::fs::write(
        vault_root.join(note_path),
        "---\ntitle: T\ntags: [unknown-tag]\n---\nBody.\n",
    )
    .expect("write note");

    let notes = vec![NoteBuilder::new(note_path).tags(&["unknown-tag"]).build()];

    let modified = migrate(vault_root, &notes, &config, false).expect("migrate");
    assert_eq!(modified, vec![note_path.to_string()]);

    let content = std::fs::read_to_string(vault_root.join(note_path)).expect("read note");
    assert!(
        !content.contains("unknown-tag"),
        "non-canonical tag should have been dropped: {content}"
    );
}

#[test]
fn rewrite_note_tags_returns_false_when_frontmatter_missing() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("no-frontmatter.md");
    let original = "Just body text, no frontmatter delimiters.\n";
    std::fs::write(&path, original).expect("write note");

    let wrote = rewrite_note_tags(&path, &["rust".to_string()]).expect("rewrite_note_tags should not error");
    assert!(!wrote, "expected false when there is no frontmatter block to rewrite");

    let content = std::fs::read_to_string(&path).expect("read note");
    assert_eq!(content, original, "bytes must be unchanged when nothing was written");
}

#[test]
fn rewrite_note_tags_returns_true_and_writes_when_frontmatter_present() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("note.md");
    std::fs::write(&path, "---\ntitle: T\ntags: [old]\n---\nBody.\n").expect("write note");

    let wrote = rewrite_note_tags(&path, &["new".to_string()]).expect("rewrite_note_tags should not error");
    assert!(wrote);

    let content = std::fs::read_to_string(&path).expect("read note");
    // P4: the writer emits block form, so an inline note is normalized on the
    // way through rather than leaving the vault with two spellings.
    assert!(content.contains("tags:\n  - new"), "expected rewritten tags: {content}");
    assert!(!content.contains("tags: ["), "inline form survived: {content}");
}

/// Implementation audit r1, M1: `migrate` caps through
/// `canonical::filter_and_cap`, which used to truncate the within-tier
/// alphabetical list with no knowledge of `no-classifier-tags`. The daemon
/// runs sweep on a cadence, so an over-cap note lost its protected tag
/// automatically. `python` sorts last of the four on purpose.
#[test]
fn migrate_does_not_drop_a_protected_tag_when_capping() {
    // See the lock comment on `migrate_excludes_paths_rewrite_note_tags_could_not_write`.
    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("assets tmpdir");
    let config = make_config(dir.path());
    std::fs::write(
        &config.canonical_path,
        "max-per-note: 3\nmax-canonical: 300\nno-classifier-tags:\n  - python\ntags:\n  ai:\n    - ai\n    - claude\n    - llm\n  tech:\n    - rust\n    - python\n",
    )
    .expect("rewrite canonical with a protect list");

    let vault_dir = tempfile::tempdir().expect("vault tmpdir");
    let vault_root = vault_dir.path();
    let note_path = "over-cap.md";
    std::fs::write(
        vault_root.join(note_path),
        "---\ntitle: T\ntags: [ai, claude, llm, python]\n---\nBody.\n",
    )
    .expect("write note");

    let notes = vec![
        NoteBuilder::new(note_path)
            .tags(&["ai", "claude", "llm", "python"])
            .build(),
    ];

    let modified = migrate(vault_root, &notes, &config, false).expect("migrate");
    assert_eq!(modified, vec![note_path.to_string()]);

    let content = std::fs::read_to_string(vault_root.join(note_path)).expect("read note");
    assert!(
        content.contains("python"),
        "sweep capped away a protected tag:\n{content}"
    );
    assert!(
        !content.contains("llm"),
        "the cap must still be hard - an unprotected tag goes instead:\n{content}"
    );
}

// ---------------------------------------------------------------------------
// Design doc `2026-09-21-staged-tag-proposals.md`, Phase 2: the proposals file
// is a rendered view of ONE scan window, written atomically, and a corrupt
// queue fails loudly instead of being silently replaced.
// ---------------------------------------------------------------------------

/// A corrupt `tag-proposals.yml` is a hand edit that went wrong. The
/// `load_proposals(path).unwrap_or(empty)` this replaces discarded it without
/// a word; the error must now reach the caller and name the file.
#[test]
fn test_write_proposals_propagates_corrupt_file() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());
    std::fs::write(&config.proposals_path, "proposals: [this is not: a list of maps\n").expect("write corrupt");

    let err = write_proposals(
        &config,
        vec![Proposal {
            tag: "novel-tag".to_string(),
            frequency: 3,
            sources: vec!["notes/a.md".to_string()],
            source: ProposalSource::Note,
        }],
        None,
    )
    .expect_err("corrupt queue must not be silently replaced");

    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("tag-proposals.yml"),
        "error must name the file, got: {rendered}"
    );
}

/// An unknown key is a schema violation, not something to ignore: `Proposal`
/// and `ProposalsFile` both carry `deny_unknown_fields`.
#[test]
fn test_write_proposals_rejects_unknown_key() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());
    std::fs::write(
        &config.proposals_path,
        "proposals:\n  - tag: old\n    frequency: 3\n    sources: []\n    action: review\n",
    )
    .expect("write legacy");

    let err = write_proposals(&config, vec![], None).expect_err("legacy `action` key must be rejected");
    assert!(format!("{err:?}").contains("tag-proposals.yml"));
}

/// Overwrite, not merge. A proposal present in the file but absent from the
/// new scan is gone after a write - including when the new scan is empty,
/// which is the case the old non-empty write gate could never express.
#[test]
fn test_write_proposals_overwrites_including_empty_scan() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let config = make_config(dir.path());

    write_proposals(
        &config,
        vec![Proposal {
            tag: "stale-tag".to_string(),
            frequency: 7,
            sources: vec!["notes/a.md".to_string()],
            source: ProposalSource::Note,
        }],
        None,
    )
    .expect("first write");
    let after_first = std::fs::read_to_string(&config.proposals_path).expect("read");
    assert!(after_first.contains("stale-tag"), "first write must land");
    assert!(after_first.contains("scanned-at"), "header must be kebab-case");

    write_proposals(&config, vec![], None).expect("second write, empty scan");
    let after_second = std::fs::read_to_string(&config.proposals_path).expect("read");
    assert!(
        !after_second.contains("stale-tag"),
        "a candidate absent from the new scan must be gone, got: {after_second}"
    );

    let parsed: ProposalsFile = serde_yaml::from_str(&after_second).expect("empty scan must parse back");
    assert!(parsed.proposals.is_empty());
    assert!(parsed.scanned_at.is_some(), "scan header must survive the round trip");
    assert!(parsed.staged_window.is_none(), "no staged arm until Phase 6");
}

/// The shipped `proposals: []` seed must still parse under the new attributes:
/// both header fields default, so the deployed file is untouched by this phase.
#[test]
fn test_shipped_empty_proposals_still_parses() {
    let parsed: ProposalsFile = serde_yaml::from_str("proposals: []\n").expect("shipped seed must parse");
    assert!(parsed.proposals.is_empty());
    assert!(parsed.scanned_at.is_none());
    assert!(parsed.staged_window.is_none());
}

// ---------------------------------------------------------------------------
// Design doc `2026-09-21-staged-tag-proposals.md`, Phase 6: the staged arm is
// wired into `scan_proposals` and reaches both call sites with no new flag.
// Counted assertions live on a FIXTURE tree; the live corpus is mutable and
// keeps only a `>=` smoke check at rollout.
// ---------------------------------------------------------------------------

/// An EMPTY receipts DB at the path `scan_proposals` resolves. The staged arm
/// treats an unreadable DB as an `Err` by design, so a test that exercises the
/// arm has to provide one; these tests are about the arm, not about identity
/// resolution, which `cortex::proposals::tests` covers against its own rows.
fn empty_receipts_db() {
    let path = vault::receipts::receipts_db_path().expect("receipts path");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("receipts dir");
    }
    let conn = rusqlite::Connection::open(&path).expect("create receipts db");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS receipts (
           trace_id TEXT NOT NULL PRIMARY KEY,
           note_path TEXT,
           raw_input TEXT NOT NULL
         );",
    )
    .expect("receipts schema");
}

fn write_staged(root: &Path, trace: &str, body: &str) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).expect("trace dir");
    std::fs::write(dir.join("distilled.yml"), body).expect("write distilled");
}

/// The whole point of the design: the note-derived arm alone finds nothing,
/// and the staged arm finds the candidate.
#[test]
fn test_scan_proposals_staged_arm_finds_what_the_note_arm_cannot() {
    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut config = make_config(dir.path());
    config.staged_proposals = true;

    empty_receipts_db();
    let staging = dir.path().join("stages");
    write_staged(&staging, "hv-1", "tags:\n  - ci-cd\n");
    write_staged(&staging, "hv-2", "tags:\n  - ci-cd\n");

    // No note carries a non-canonical tag: this is the tags-only steady state.
    let notes = vec![NoteBuilder::new("notes/a.md").tags(&["rust"]).build()];

    let off = {
        let mut c = make_config(dir.path());
        c.staged_proposals = false;
        scan_proposals(dir.path(), &notes, &c, &staging).expect("note-only scan")
    };
    assert!(off.proposals.is_empty(), "note-derived arm alone is empty: {off:?}");
    assert!(!off.staged_scanned, "staged-proposals: false must not scan");

    let on = scan_proposals(dir.path(), &notes, &config, &staging).expect("staged scan");
    assert!(on.staged_scanned);
    assert_eq!(on.staged_traces, 2);
    assert!(
        on.proposals.iter().any(|p| p.tag == "ci-cd"),
        "the staged arm must surface ci-cd: {:?}",
        on.proposals
    );
}

/// A host with no staging tree (the laptop: it POSTs to the daemon and owns no
/// staging root) runs the note-only scan and exits 0, reporting the gap rather
/// than silently looking like a clean scan.
#[test]
fn test_scan_proposals_without_a_staging_root_is_note_only_not_an_error() {
    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut config = make_config(dir.path());
    config.staged_proposals = true;

    let scan = scan_proposals(dir.path(), &[], &config, &dir.path().join("no-such-root")).expect("must not fail");
    assert!(!scan.staged_scanned, "coverage is reported, not inferred");
    assert!(scan.proposals.is_empty());
}

/// The scan window reaches the written file, so a reader can tell what period
/// the frequencies cover.
#[test]
fn test_staged_window_is_written_to_the_queue() {
    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut config = make_config(dir.path());
    config.staged_proposals = true;

    empty_receipts_db();
    let staging = dir.path().join("stages");
    write_staged(
        &staging,
        "hv-1",
        "tags:\n  - ci-cd\n  - jsonl\nmeta:\n  produced-at: 2026-08-05T00:00:00Z\n",
    );
    write_staged(
        &staging,
        "hv-2",
        "tags:\n  - ci-cd\n  - jsonl\nmeta:\n  produced-at: 2026-09-21T00:00:00Z\n",
    );

    let scan = scan_proposals(dir.path(), &[], &config, &staging).expect("scan");
    write_proposals(&config, scan.proposals.clone(), scan.staged_window.clone()).expect("write");

    let text = std::fs::read_to_string(&config.proposals_path).expect("read");
    let parsed: ProposalsFile = serde_yaml::from_str(&text).expect("must parse back through ProposalsFile");
    assert_eq!(
        parsed.staged_window.as_deref(),
        Some("2026-08-05T00:00:00Z .. 2026-09-21T00:00:00Z")
    );
    assert!(parsed.scanned_at.is_some());
    assert!(!parsed.proposals.is_empty());
}

/// A staging root that EXISTS but cannot be enumerated is an Err, and the
/// prior queue is left byte-identical. Phase 2 made the write unconditional,
/// so a reader that could not distinguish "could not look" from "nothing
/// found" would be a queue-wipe path.
#[test]
#[cfg(unix)]
fn test_unreadable_staging_root_errors_and_preserves_the_queue() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut config = make_config(dir.path());
    config.staged_proposals = true;

    write_proposals(
        &config,
        vec![Proposal {
            tag: "keep-me".to_string(),
            frequency: 4,
            sources: vec!["notes/a.md".to_string()],
            source: ProposalSource::Note,
        }],
        None,
    )
    .expect("seed the queue");
    let before = std::fs::read(&config.proposals_path).expect("read before");

    let staging = dir.path().join("locked");
    std::fs::create_dir_all(&staging).expect("mkdir");
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let result = scan_proposals(dir.path(), &[], &config, &staging);
    // Restore before asserting so the tempdir can always be cleaned up.
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).expect("restore");

    let err = result.expect_err("an unreadable staging root must not read as an empty scan");
    assert!(format!("{err:?}").contains("staging root"), "{err:?}");

    let after = std::fs::read(&config.proposals_path).expect("read after");
    assert_eq!(before, after, "the prior queue must be untouched on Err");
}

/// The template's `staged-proposals` key is asserted on its PARSED VALUE, not
/// merely that the file parses: `SweepConfig` has no `deny_unknown_fields`, so
/// "it parses" would pass for a misspelled key and prove nothing.
#[test]
fn test_staged_proposals_key_parses_to_its_value() {
    let on: crate::config::Config = serde_yaml::from_str("sweep:\n  staged-proposals: true\n").expect("parse");
    assert!(on.sweep.staged_proposals);
    let off: crate::config::Config = serde_yaml::from_str("sweep:\n  staged-proposals: false\n").expect("parse");
    assert!(!off.sweep.staged_proposals, "the key must be honored, not defaulted");
    let absent: crate::config::Config = serde_yaml::from_str("log-level: info\n").expect("parse");
    assert!(absent.sweep.staged_proposals, "default is on");
}
