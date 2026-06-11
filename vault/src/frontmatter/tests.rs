use super::*;

#[test]
fn test_parse_frontmatter_basic() {
    let raw = "---\ntitle: Test\ntype: note\n---\nBody text.";
    let (fm, body) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.title.as_deref(), Some("Test"));
    assert_eq!(fm.note_type.as_deref(), Some("note"));
    assert_eq!(body, "Body text.");
}

#[test]
fn test_parse_frontmatter_none() {
    let raw = "Just some text without frontmatter.";
    let (fm, body) = parse_frontmatter(raw).expect("parse");
    assert!(fm.is_empty());
    assert_eq!(body, raw);
}

#[test]
fn test_parse_frontmatter_no_closing() {
    let raw = "---\ntitle: Test\nNo closing delimiter.";
    let (fm, _body) = parse_frontmatter(raw).expect("parse");
    assert!(fm.is_empty());
}

#[test]
fn test_frontmatter_roundtrip() {
    let fm = Frontmatter {
        title: Some("Test".to_string()),
        date: Some("2026-01-01".to_string()),
        note_type: Some("note".to_string()),
        domain: Some("tech".to_string()),
        origin: Some("authored".to_string()),
        tags: Some(vec!["rust".to_string()]),
        ..Default::default()
    };

    let yaml = fm.to_yaml().expect("to_yaml");
    assert!(yaml.contains("title: Test"));
    assert!(yaml.contains("domain: tech"));
}

#[test]
fn test_frontmatter_extra_fields() {
    let raw = "---\ntitle: Test\ncustom: value\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.title.as_deref(), Some("Test"));
    assert!(fm.extra.contains_key("custom"));
}

#[test]
fn pinned_roundtrips_through_yaml() {
    let raw = "---\ntitle: P\npinned: true\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.pinned, Some(true));

    let emitted = fm.to_yaml().expect("to_yaml");
    assert!(emitted.contains("pinned: true"), "yaml omitted pinned: {emitted}");

    // Round-trip back.
    let raw2 = format!("---\n{emitted}---\n");
    let (fm2, _) = parse_frontmatter(&raw2).expect("reparse");
    assert_eq!(fm2.pinned, Some(true));
}

#[test]
fn pinned_missing_is_none() {
    let raw = "---\ntitle: X\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert!(fm.pinned.is_none());
}

#[test]
fn pinned_strict_bool_only() {
    // pinned: "true" (string), pinned: 1 (int), and a null pinned: all
    // resolve to None rather than `Some` of something weird. Indexing
    // these as 0 keeps a typo from breaking reindex.
    let raw = "---\ntitle: X\npinned: \"true\"\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("string");
    assert_eq!(fm.pinned, None, "string value must not parse as Some");

    let raw = "---\ntitle: X\npinned: 1\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("int");
    assert_eq!(fm.pinned, None, "int value must not parse as Some");

    let raw = "---\ntitle: X\npinned: ~\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("null");
    assert_eq!(fm.pinned, None, "explicit null must not parse as Some");
}

#[test]
fn test_frontmatter_is_empty() {
    assert!(Frontmatter::default().is_empty());
    assert!(
        !Frontmatter {
            title: Some("x".to_string()),
            ..Default::default()
        }
        .is_empty()
    );
}

#[test]
fn scalar_number_coerces_to_plain_text_not_debug() {
    // `date: 2023` is a bare YAML integer; it must store "2023", not
    // the old `"Number(2023)"` debug rendering of the Value enum.
    let raw = "---\ndate: 2023\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.date.as_deref(), Some("2023"));
}

#[test]
fn scalar_bool_coerces_to_plain_text() {
    let raw = "---\nstatus: true\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.status.as_deref(), Some("true"));
}

#[test]
fn scalar_null_yields_none() {
    let raw = "---\ntitle: ~\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.title, None);
}

#[test]
fn scalar_sequence_yields_none() {
    // A sequence value on a string-typed field is not coercible to a
    // scalar string; it drops to None rather than a debug rendering.
    let raw = "---\nsource:\n  - a\n  - b\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.source, None);
}

#[test]
fn scalar_string_passes_through() {
    let raw = "---\ndate: 2023-01-13\ntitle: Hello\n---\nBody.";
    let (fm, _) = parse_frontmatter(raw).expect("parse");
    assert_eq!(fm.date.as_deref(), Some("2023-01-13"));
    assert_eq!(fm.title.as_deref(), Some("Hello"));
}
