use super::*;

#[test]
fn cli_ingests_do_not_ping_the_operator() {
    assert!(!pings_operator(IngestMethod::Cli));
}

#[test]
fn human_transports_still_ping_the_operator() {
    for method in [
        IngestMethod::Telegram,
        IngestMethod::Discord,
        IngestMethod::Http,
        IngestMethod::Clipboard,
        IngestMethod::Ntfy,
        IngestMethod::Signal,
        IngestMethod::Manual,
    ] {
        assert!(pings_operator(method), "{method:?} must still ping");
    }
}
