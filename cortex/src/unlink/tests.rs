use super::*;

use crate::config::VaultConfig;

fn stopwords(list: &[&str]) -> Stopwords {
    Stopwords::new(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

fn vault_config() -> VaultConfig {
    VaultConfig {
        root_path: None,
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
        exclude: Vec::new(),
        include: Vec::new(),
    }
}

/// A temp vault with the given `(relative_path, contents)` files.
fn vault_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, contents) in files {
        let abs = dir.path().join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&abs, contents).expect("write note");
    }
    dir
}

fn sweep(dir: &tempfile::TempDir, stop: &Stopwords, apply: bool) -> UnlinkStats {
    let notes = crate::vault::scan_vault(dir.path(), &vault_config()).expect("scan");
    run_with_notes(dir.path(), &notes, stop, apply, false).expect("sweep")
}

fn read(dir: &tempfile::TempDir, path: &str) -> String {
    std::fs::read_to_string(dir.path().join(path)).expect("read back")
}

// --- the core retraction ---------------------------------------------------

#[test]
fn retracts_bare_and_piped_links_preserving_what_the_reader_sees() {
    let dir = vault_with(&[(
        "notes/a.md",
        "---\ntitle: A\ntype: note\n---\nread [[Every]] daily and [[every|Every]] week\n",
    )]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(
        read(&dir, "notes/a.md"),
        "---\ntitle: A\ntype: note\n---\nread Every daily and Every week\n",
        "bare links keep their own case; piped links keep their display text"
    );
    assert_eq!(stats.files_changed, 1);
    assert_eq!(stats.occurrences, 2);
    assert_eq!(
        stats.changes,
        vec![UnlinkChange {
            path: std::path::PathBuf::from("notes/a.md"),
            target: "Every".to_string(),
            occurrences: 2,
        }],
        "occurrences for one target in one note collapse into a single change row"
    );
}

#[test]
fn leaves_every_link_that_is_not_stoplisted_alone() {
    let body = "see [[rust-guide]] and [[python-guide|the Python guide]]\n";
    let dir = vault_with(&[("notes/a.md", &format!("---\ntitle: A\n---\n{body}"))]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(read(&dir, "notes/a.md"), format!("---\ntitle: A\n---\n{body}"));
    assert_eq!(stats.files_changed, 0);
    assert_eq!(stats.occurrences, 0);
}

#[test]
fn frontmatter_is_preserved_byte_for_byte() {
    // The sweep splits on the RAW file, so the blank line after the closing
    // `---` and any exotic YAML survive untouched. Reassembling from the
    // trimmed `Note::body` would silently eat that newline in every file.
    let raw = "---\ntitle: A\ntags: [x]\nnote: |\n  a block scalar\n  with lines\n---\n\n\nread [[every]] day\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    sweep(&dir, &stopwords(&["every"]), true);

    let after = read(&dir, "notes/a.md");
    let (before_fm, _) = raw.split_once("---\n\n\n").expect("split fixture");
    assert!(after.starts_with(before_fm), "frontmatter prefix unchanged");
    assert_eq!(
        after, "---\ntitle: A\ntags: [x]\nnote: |\n  a block scalar\n  with lines\n---\n\n\nread every day\n",
        "only the link markup changed"
    );
}

#[test]
fn a_dry_run_reports_everything_and_writes_nothing() {
    let raw = "---\ntitle: A\n---\nread [[every]] day\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), false);

    assert!(!stats.applied);
    assert_eq!(stats.files_changed, 1, "the dry run still reports the file");
    assert_eq!(stats.occurrences, 1);
    assert_eq!(read(&dir, "notes/a.md"), raw, "nothing was written");
}

#[test]
fn is_idempotent() {
    let dir = vault_with(&[("notes/a.md", "---\ntitle: A\n---\nread [[every]] day\n")]);

    let first = sweep(&dir, &stopwords(&["every"]), true);
    let after_first = read(&dir, "notes/a.md");
    let second = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(first.occurrences, 1);
    assert_eq!(second.occurrences, 0, "steady state: nothing left to retract");
    assert_eq!(read(&dir, "notes/a.md"), after_first, "bytes unchanged on the rerun");
}

#[test]
fn an_empty_vocabulary_is_a_no_op_not_a_vault_wide_rewrite() {
    let raw = "---\ntitle: A\n---\nread [[every]] day\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &Stopwords::default(), true);

    assert_eq!(stats.files_changed, 0);
    assert_eq!(stats.scanned, 0, "the empty list short-circuits before scanning");
    assert_eq!(read(&dir, "notes/a.md"), raw);
}

// --- scope: retract only what the linker could have written -----------------

#[test]
fn skips_authored_notes_because_the_linker_never_wrote_there() {
    let raw = "---\ntitle: A\norigin: authored\n---\nmy own [[every]] link\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(stats.skipped_authored, 1);
    assert_eq!(stats.files_changed, 0);
    assert_eq!(read(&dir, "notes/a.md"), raw, "Scott's own prose is his to link");
}

#[test]
fn include_authored_opts_into_authored_notes() {
    // The linker's authored exemption (fa3f9a8, 2026-06-12) landed AFTER
    // `entities/every.md` was minted (2026-06-11), so links it wrote into
    // authored notes during that window are damage, not prose. Opt-in.
    let dir = vault_with(&[(
        "notes/a.md",
        "---\ntitle: A\norigin: \"authored\"\n---\nlinker damage [[every]] here\n",
    )]);

    let notes = crate::vault::scan_vault(dir.path(), &vault_config()).expect("scan");
    let stats = run_with_notes(dir.path(), &notes, &stopwords(&["every"]), true, true).expect("sweep");

    assert_eq!(stats.skipped_authored, 0, "the exemption is waived");
    assert_eq!(stats.occurrences, 1);
    assert!(read(&dir, "notes/a.md").contains("linker damage every here"));
}

#[test]
fn skips_hub_bodies_because_the_hub_builder_owns_those_bytes() {
    let raw = "---\ntitle: Every\ntype: entity\n---\nclaim mentioning [[every]] here\n";
    let dir = vault_with(&[(&format!("{}/every.md", crate::hub::HUB_DIR), raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(stats.skipped_hub, 1);
    assert_eq!(stats.files_changed, 0);
    assert_eq!(read(&dir, &format!("{}/every.md", crate::hub::HUB_DIR)), raw);
}

// --- constructs the sweep must not touch -----------------------------------

#[test]
fn leaves_links_inside_code_alone() {
    // The linker refuses to WRITE into code, so a `[[every]]` in a fence or
    // an inline span is source material a reader is meant to see verbatim.
    let raw = concat!(
        "---\ntitle: A\n---\n",
        "prose [[every]] here\n",
        "\n```md\nsample [[every]] link\n```\n",
        "\ninline `[[every]]` span\n",
    );
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    let after = read(&dir, "notes/a.md");
    assert_eq!(stats.occurrences, 1, "only the prose occurrence retracts");
    assert!(after.contains("prose every here"));
    assert!(after.contains("sample [[every]] link"), "fenced block untouched");
    assert!(after.contains("inline `[[every]]` span"), "inline code untouched");
}

#[test]
fn leaves_transclusions_alone() {
    // `![[every]]` embeds the note; unwrapping it changes what RENDERS, not
    // just how it links - and the auto-linker never writes an embed.
    let raw = "---\ntitle: A\n---\n![[every]]\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(stats.occurrences, 0);
    assert_eq!(read(&dir, "notes/a.md"), raw);
}

#[test]
fn leaves_heading_and_block_refs_alone() {
    // `[[every#section]]` is a different target than `every`; the graph layer
    // does not stoplist it either, so neither does the sweep.
    let raw = "---\ntitle: A\n---\nsee [[every#section]] and [[every^abc]]\n";
    let dir = vault_with(&[("notes/a.md", raw)]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(stats.occurrences, 0);
    assert_eq!(read(&dir, "notes/a.md"), raw);
}

#[test]
fn matches_the_target_case_insensitively_but_never_a_substring() {
    let dir = vault_with(&[(
        "notes/a.md",
        "---\ntitle: A\n---\n[[EVERY]] and [[everyone]] and [[every-thing]]\n",
    )]);

    let stats = sweep(&dir, &stopwords(&["every"]), true);

    assert_eq!(stats.occurrences, 1);
    assert_eq!(
        read(&dir, "notes/a.md"),
        "---\ntitle: A\n---\nEVERY and [[everyone]] and [[every-thing]]\n"
    );
}

// --- the retract primitive directly ----------------------------------------

#[test]
fn retract_reports_per_target_counts_in_first_seen_order() {
    let stop = stopwords(&["every", "brief"]);
    let (out, hits) = retract("[[brief]] then [[every]] then [[every]]", &stop);

    assert_eq!(out, "brief then every then every");
    assert_eq!(
        hits,
        vec![("brief".to_string(), 1), ("every".to_string(), 2)],
        "first-seen order, one row per target"
    );
}

#[test]
fn retract_leaves_a_body_with_no_stoplisted_links_byte_identical() {
    let stop = stopwords(&["every"]);
    let body = "plain prose with [[rust-guide]] and no stoplisted target";
    let (out, hits) = retract(body, &stop);

    assert_eq!(out, body);
    assert!(hits.is_empty());
}
