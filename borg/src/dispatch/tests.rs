use super::*;

#[test]
fn cli_ingests_do_not_ping_the_operator() {
    assert!(!pings_operator(IngestMethod::Cli, None));
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
        assert!(pings_operator(method, None), "{method:?} must still ping");
    }
}

#[test]
fn an_auto_remediation_never_pings_whatever_method_it_carries() {
    for method in IngestMethod::all() {
        assert!(
            !pings_operator(*method, Some(ReplayOrigin::AutoRemediation)),
            "{method:?} remediation must not ping"
        );
    }
}
