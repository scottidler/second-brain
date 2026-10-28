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

/// `dispatch_ingest`'s own gate, driven with live sinks: a `cli` dispatch
/// attempts no processing or result ping, an `http` one attempts both on both
/// sinks. The pipeline fails offline at Stage 0 (staging root under a file).
#[tokio::test]
async fn dispatch_ingest_sends_no_ping_for_cli_and_both_for_http() {
    let data_home = tempfile::tempdir().expect("tempdir");
    crate::harvest::with_xdg_data_home(data_home.path(), || async {
        // Idempotent; the daemon inits this at startup.
        crate::pipeline::permits::GENERAL_PERMITS.init(4);
        let blocker = tempfile::tempdir().expect("tempdir");
        let file = blocker.path().join("not-a-dir");
        std::fs::write(&file, b"x").expect("blocker file");
        let mut config = Config::default();
        config.staging.enabled = true;
        config.staging.root = file.join("stages");
        let telegram = Telegram::new(
            "0:test",
            &crate::config::TelegramConfig {
                bot_token: String::new(),
                allowed_chat_ids: vec![],
                notification_chat_id: Some(1),
                host: None,
            },
        );
        let desktop = Desktop::new(&crate::config::DesktopConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(telegram.is_some() && desktop.is_some());

        for (method, want) in [(IngestMethod::Cli, 0), (IngestMethod::Http, 2)] {
            let trace = crate::trace::generate(method);
            let url = format!("https://example.com/dispatch-{}", uuid::Uuid::new_v4());
            dispatch_ingest(
                ContentKind::Url {
                    url: url.clone(),
                    note: None,
                },
                vec![],
                method,
                false,
                &config,
                trace.clone(),
                &url,
                "Processing...",
                desktop.clone(),
                telegram.clone(),
                None,
            )
            .await;
            assert_eq!(
                crate::notify::probe::attempts(&trace),
                want,
                "{method:?} processing pings"
            );
            assert_eq!(crate::notify::probe::attempts(&url), want, "{method:?} result pings");
        }
    })
    .await;
}
