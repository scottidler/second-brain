use super::*;

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date")
}

/// A vault with Obsidian's daily-notes settings and a template, as this vault has them.
fn vault(template: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".obsidian")).expect("obsidian dir");
    std::fs::write(
        dir.path().join(DAILY_NOTES_SETTINGS),
        r#"{"folder": "journal", "format": "YYYY/MM/YYYY-MM-DD", "template": "system/templates/daily"}"#,
    )
    .expect("settings");
    std::fs::create_dir_all(dir.path().join("system/templates")).expect("templates dir");
    std::fs::write(dir.path().join("system/templates/daily.md"), template).expect("template");
    dir
}

const TEMPLATE: &str =
    "---\ntitle: \"{{date:YYYY-MM-DD}}\"\ndate: {{date:YYYY-MM-DD}}\ntype: daily\nweight:\n---\n\n# daily\n";

#[test]
fn creates_the_note_at_obsidians_path_with_dates_rendered() {
    let dir = vault(TEMPLATE);
    let outcome = ensure(dir.path(), date("2026-10-24")).expect("ensure");
    let path = dir.path().join("journal/2026/10/2026-10-24.md");
    assert_eq!(outcome, Ensured::Created(path.clone()));
    let body = std::fs::read_to_string(&path).expect("note");
    assert_eq!(
        body,
        "---\ntitle: \"2026-10-24\"\ndate: 2026-10-24\ntype: daily\nweight:\n---\n\n# daily\n"
    );
}

#[test]
fn an_existing_note_is_left_byte_identical() {
    let dir = vault(TEMPLATE);
    let path = dir.path().join("journal/2026/10/2026-10-24.md");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(&path, "my own words\n").expect("existing note");
    let outcome = ensure(dir.path(), date("2026-10-24")).expect("ensure");
    assert_eq!(outcome, Ensured::Exists(path.clone()));
    assert_eq!(std::fs::read_to_string(&path).expect("note"), "my own words\n");
}

#[test]
fn a_second_ensure_is_a_no_op() {
    let dir = vault(TEMPLATE);
    let first = ensure(dir.path(), date("2026-10-24")).expect("first");
    let second = ensure(dir.path(), date("2026-10-24")).expect("second");
    assert!(matches!(first, Ensured::Created(_)));
    assert!(matches!(second, Ensured::Exists(_)));
}

#[test]
fn missing_settings_is_an_error_not_a_guessed_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = ensure(dir.path(), date("2026-10-24")).expect_err("no settings");
    assert!(format!("{err:#}").contains("daily-notes.json"), "{err:#}");
    assert!(!dir.path().join("journal").exists());
}

#[test]
fn an_unsupported_template_token_is_an_error_and_writes_nothing() {
    let dir = vault("---\ndate: {{time}}\n---\n");
    let err = ensure(dir.path(), date("2026-10-24")).expect_err("unsupported token");
    assert!(format!("{err:#}").contains("time"), "{err:#}");
    assert!(!dir.path().join("journal/2026/10/2026-10-24.md").exists());
}

#[test]
fn bare_date_and_title_tokens_render() {
    let out = render("{{date}} {{title}}", date("2026-01-05"), "2026-01-05").expect("render");
    assert_eq!(out, "2026-01-05 2026-01-05");
}

#[test]
fn format_date_rejects_tokens_it_does_not_know() {
    assert!(format_date("YYYY-MMM-DD", date("2026-10-24")).is_err());
    assert!(format_date("dddd", date("2026-10-24")).is_err());
}
