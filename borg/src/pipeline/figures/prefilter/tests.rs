use super::*;

fn cfg() -> ImagesConfig {
    ImagesConfig::default()
}

const PNG: &str = "image/png";

fn run(source: Source, url: &str, mime: &str, dims: Option<(u32, u32)>) -> Verdict {
    prefilter(&cfg(), source, url, mime, dims, b"bytes")
}

#[test]
fn table_drops_decoration_and_unsupported_formats_and_keeps_diagrams() {
    let web = Source::WebArticle;
    let cases: Vec<(&str, Verdict)> = vec![
        // 90x90 XDA avatar: under min-side-px.
        (
            "xda avatar 90x90",
            run(web, "https://www.xda-developers.com/img/a-b.jpg", PNG, Some((90, 90))),
        ),
        // 420px logo: big enough, dropped by the URL token alone.
        (
            "logo 420px",
            run(web, "https://example.com/assets/logo.png", PNG, Some((420, 420))),
        ),
        (
            "data uri",
            run(web, "data:image/png;base64,AAAA", PNG, Some((400, 400))),
        ),
        (
            "avif",
            run(web, "https://example.com/x.avif", "image/avif", Some((400, 400))),
        ),
        (
            "bmp",
            run(web, "https://example.com/x.bmp", "image/bmp", Some((400, 400))),
        ),
        (
            "martinfowler diagram",
            run(
                web,
                "https://martinfowler.com/articles/platform-teams-stuff-done/platform-teams-stuff-done.png",
                PNG,
                Some((1200, 800)),
            ),
        ),
        (
            "silicon is not icon",
            run(web, "https://example.com/silicon-valley-map.png", PNG, Some((800, 600))),
        ),
        (
            "x photo 150px",
            run(
                Source::XPhoto,
                "https://pbs.twimg.com/media/x.jpg",
                "image/jpeg",
                Some((150, 150)),
            ),
        ),
    ];
    let expected = [
        Verdict::Drop(DropReason::TooSmall),
        Verdict::Drop(DropReason::UrlToken),
        Verdict::Drop(DropReason::UnsupportedFormat),
        Verdict::Drop(DropReason::UnsupportedFormat),
        Verdict::Drop(DropReason::UnsupportedFormat),
        Verdict::Keep,
        Verdict::Keep,
        Verdict::Keep,
    ];
    for ((name, got), want) in cases.into_iter().zip(expected) {
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn content_heuristics_do_not_apply_to_x_sources() {
    for source in [Source::XPhoto, Source::XArticle] {
        assert_eq!(
            run(source, "https://pbs.twimg.com/logo/icon.png", PNG, Some((20, 20))),
            Verdict::Keep,
            "{source:?}"
        );
    }
}

#[test]
fn format_checks_apply_to_every_source() {
    for source in [Source::XPhoto, Source::XArticle, Source::WebArticle] {
        assert_eq!(
            run(source, "https://example.com/x.bmp", "image/bmp", Some((500, 500))),
            Verdict::Drop(DropReason::UnsupportedFormat),
            "{source:?}"
        );
        assert_eq!(
            run(source, "data:image/png;base64,AAAA", PNG, Some((500, 500))),
            Verdict::Drop(DropReason::UnsupportedFormat),
            "{source:?}"
        );
    }
}

#[test]
fn url_tokens_match_whole_tokens_in_host_and_path_only() {
    let web = Source::WebArticle;
    let big = Some((800, 600));
    for url in [
        "https://example.com/Avatars/me.png",
        "https://example.com/img/favicon.png",
        "https://example.com/a/emoji-1.png",
        "https://example.com/tracking/pixel.gif",
        "https://logo.example.com/x.png",
        "https://yt3.ggpht.com/abc",
        "https://img.shields.io/badge/x-y-green.png",
    ] {
        assert_eq!(run(web, url, PNG, big), Verdict::Drop(DropReason::UrlToken), "{url}");
    }
    // Substring hits and a query-only token are not decoration.
    for url in [
        "https://example.com/silicon.png",
        "https://example.com/iconic-diagram.png",
        "https://example.com/d.png?logo=1",
    ] {
        assert_eq!(run(web, url, PNG, big), Verdict::Keep, "{url}");
    }
}

#[test]
fn web_image_with_unknown_size_is_dropped_and_x_is_kept() {
    assert_eq!(
        run(Source::WebArticle, "https://example.com/d.png", PNG, None),
        Verdict::Drop(DropReason::Undecodable)
    );
    assert_eq!(
        run(Source::XPhoto, "https://pbs.twimg.com/x.jpg", "image/jpeg", None),
        Verdict::Keep
    );
}

#[test]
fn min_side_applies_to_either_axis_and_is_inclusive_at_the_bound() {
    let web = Source::WebArticle;
    assert_eq!(
        run(web, "https://example.com/d.png", PNG, Some((2000, 199))),
        Verdict::Drop(DropReason::TooSmall)
    );
    assert_eq!(
        run(web, "https://example.com/d.png", PNG, Some((200, 200))),
        Verdict::Keep
    );
}

#[test]
fn empty_body_is_dropped() {
    assert_eq!(
        prefilter(&cfg(), Source::XPhoto, "https://example.com/d.png", PNG, None, b""),
        Verdict::Drop(DropReason::Empty)
    );
}

#[test]
fn dedupe_drops_exact_repeats_only() {
    let mut dedupe = Dedupe::default();
    assert!(dedupe.first_seen(b"chart one"));
    assert!(!dedupe.first_seen(b"chart one"));
    assert!(dedupe.first_seen(b"chart two"));
}

#[test]
fn prefilter_url_drops_before_download() {
    assert_eq!(
        prefilter_url(Source::WebArticle, "data:image/gif;base64,R0lGOD"),
        Verdict::Drop(DropReason::UnsupportedFormat)
    );
    assert_eq!(
        prefilter_url(Source::WebArticle, "https://example.com/logo.png"),
        Verdict::Drop(DropReason::UrlToken)
    );
    assert_eq!(
        prefilter_url(Source::XPhoto, "https://pbs.twimg.com/logo.png"),
        Verdict::Keep
    );
    assert_eq!(
        prefilter_url(Source::XPhoto, "not a url"),
        Verdict::Drop(DropReason::UnsupportedFormat)
    );
}
