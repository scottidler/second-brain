use super::*;

#[test]
fn is_permanent_is_true_for_exactly_not_found_and_not_live() {
    let permanent = [FetchError::NotFound("id".into()), FetchError::NotLive("id".into())];
    let transient = [
        FetchError::Auth("a".into()),
        FetchError::Quota("q".into()),
        FetchError::Service("s".into()),
        FetchError::Network("n".into()),
        FetchError::Response("r".into()),
        FetchError::Empty,
        FetchError::Timeout,
        FetchError::YtDlp(eyre::eyre!("boom")),
    ];
    assert!(permanent.iter().all(FetchError::is_permanent));
    assert!(transient.iter().all(|e| !e.is_permanent()));
}

#[test]
fn not_found_reason_names_the_video() {
    assert_eq!(
        FetchError::NotFound("abc".into()).to_string(),
        "youtube data api: video abc not found"
    );
}

#[test]
fn yt_dlp_wraps_the_report_unchanged() {
    let e = FetchError::YtDlp(eyre::eyre!("Video unavailable"));
    assert!(e.to_string().contains("Video unavailable"));
}

#[test]
fn redact_scrubs_the_secret_from_every_text_variant() {
    let secret = "AIza-SECRET-123";
    let text = format!("API key not valid: {secret} (again {secret})");
    let variants = [
        FetchError::NotFound(text.clone()),
        FetchError::NotLive(text.clone()),
        FetchError::Auth(text.clone()),
        FetchError::Quota(text.clone()),
        FetchError::Service(text.clone()),
        FetchError::Network(text.clone()),
        FetchError::Response(text.clone()),
    ];
    for e in variants {
        let shown = e.redact(secret).to_string();
        assert!(!shown.contains(secret), "{shown}");
        assert!(shown.contains(REDACTED), "{shown}");
    }
}

#[test]
fn redact_with_a_blank_secret_changes_nothing() {
    let e = FetchError::Auth("plain reason".into()).redact("  ");
    assert_eq!(e.to_string(), "youtube auth: plain reason");
    assert!(matches!(FetchError::Timeout.redact("k"), FetchError::Timeout));
}
