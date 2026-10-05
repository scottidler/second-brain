use super::*;
use vault::queue::QueueSnapshot;

fn idle() -> QueueSnapshot {
    QueueSnapshot::idle()
}

#[test]
fn resolve_format_tty_defaults_yaml_pipe_defaults_json() {
    assert_eq!(resolve_format(None, true), Format::Yaml);
    assert_eq!(resolve_format(None, false), Format::Json);
}

#[test]
fn resolve_format_explicit_beats_tty_detection() {
    assert_eq!(resolve_format(Some(Format::Json), true), Format::Json);
    assert_eq!(resolve_format(Some(Format::Yaml), false), Format::Yaml);
}

#[test]
fn idle_yaml_is_exactly_state_idle() {
    assert_eq!(render(&idle(), Format::Yaml).expect("yaml"), "state: idle\n");
}

#[test]
fn idle_json_is_exactly_state_idle_object() {
    assert_eq!(render(&idle(), Format::Json).expect("json"), "{\"state\":\"idle\"}\n");
}

#[test]
fn format_parses_case_insensitively() {
    assert_eq!(Format::from_str("YAML", true).expect("YAML"), Format::Yaml);
    assert_eq!(Format::from_str("Json", true).expect("Json"), Format::Json);
    assert!(Format::from_str("toml", true).is_err());
}
