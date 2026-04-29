#![allow(clippy::unwrap_used)]

use super::*;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn parse_duration_days() {
    assert_eq!(parse_duration("7d").unwrap(), Duration::days(7));
}

#[test]
fn parse_duration_hours() {
    assert_eq!(parse_duration("24h").unwrap(), Duration::hours(24));
}

#[test]
fn parse_duration_minutes() {
    assert_eq!(parse_duration("30m").unwrap(), Duration::minutes(30));
}

#[test]
fn parse_duration_seconds() {
    assert_eq!(parse_duration("90s").unwrap(), Duration::seconds(90));
}

#[test]
fn parse_duration_rejects_bare_number() {
    assert!(parse_duration("7").is_err());
}

#[test]
fn parse_duration_rejects_empty() {
    assert!(parse_duration("").is_err());
}

#[test]
fn read_source_from_note_extracts_url() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nsource: https://example.com/article\ntags: []\n---\nbody"
    )
    .unwrap();
    let source = read_source_from_note(file.path()).unwrap();
    assert_eq!(source, "https://example.com/article");
}

#[test]
fn read_source_from_note_errors_on_missing_source() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\n---\nbody").unwrap();
    let result = read_source_from_note(file.path());
    assert!(result.is_err());
}

#[test]
fn read_source_from_note_errors_on_no_frontmatter() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "Just a plain markdown file.").unwrap();
    let result = read_source_from_note(file.path());
    assert!(result.is_err());
}

#[test]
fn read_source_handles_quoted_value() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nsource: \"https://example.com/a b\"\n---\nbody"
    )
    .unwrap();
    let source = read_source_from_note(file.path()).unwrap();
    assert_eq!(source, "https://example.com/a b");
}

#[test]
fn read_method_from_note_extracts_telegram() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nmethod: telegram\nsource: https://example.com\n---\nbody"
    )
    .unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("telegram".to_string()));
}

#[test]
fn read_method_from_note_extracts_cli() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(
        file,
        "---\ntitle: Example\nmethod: cli\nsource: https://example.com\n---\nbody"
    )
    .unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("cli".to_string()));
}

#[test]
fn read_method_from_note_returns_none_when_missing() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\nsource: https://example.com\n---\nbody").unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, None);
}

#[test]
fn read_method_from_note_handles_quoted_value() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "---\ntitle: Example\nmethod: \"telegram\"\n---\nbody").unwrap();
    let method = read_method_from_note(file.path()).unwrap();
    assert_eq!(method, Some("telegram".to_string()));
}
