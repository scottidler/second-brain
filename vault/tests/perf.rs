//! Performance harness for vault::note::scan_vault.
//!
//! Marked `#[ignore]` so it does not run on `cargo test` by default. Run with
//! `cargo test --package vault --test perf -- --ignored --nocapture` to see timings.
//!
//! The harness builds a tempdir vault of `NOTE_COUNT` markdown files with frontmatter and reports
//! wall-clock time for a single `scan_vault` call. Use this to compare before/after timings when
//! evaluating the par_iter conversion (Phase 1 of the rayon design doc).

use std::fs;
use std::time::Instant;
use vault::config::ScanConfig;
use vault::note::scan_vault;

const NOTE_COUNT: usize = 1000;

#[test]
#[ignore]
fn perf_scan_vault_thousand_notes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    let filler = "Filler body line. ".repeat(50);
    for i in 0..NOTE_COUNT {
        let path = root.join(format!("note-{i:04}.md"));
        let body = format!(
            "---\ntitle: Note {i}\ntype: knowledge\ndomain: tools\norigin: authored\nstatus: draft\nmethod: cli\ntags:\n  - rust\n  - perf\n---\n# Note {i}\n\n{filler}\n"
        );
        fs::write(&path, body).expect("write note");
    }

    let scan_config = ScanConfig {
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
    };

    let start = Instant::now();
    let notes = scan_vault(root, &scan_config).expect("scan");
    let elapsed = start.elapsed();

    assert_eq!(notes.len(), NOTE_COUNT, "expected all notes to parse");
    println!(
        "scan_vault({NOTE_COUNT} notes) -> {} parsed in {:?}",
        notes.len(),
        elapsed
    );
}
