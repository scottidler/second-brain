use super::*;

const PAGE: &str = "https://martinfowler.com/articles/platform-teams-stuff-done.html";

fn urls(md: &str) -> Vec<String> {
    parse_markdown_images(md, PAGE).into_iter().map(|i| i.url).collect()
}

#[test]
fn plain_images_come_out_in_order_with_alt() {
    let md = "intro ![first](https://a.example/1.png) mid ![](https://a.example/2.jpg \"a title\") end";
    let got = parse_markdown_images(md, PAGE);
    assert_eq!(got.len(), 2);
    assert_eq!(
        (got[0].alt.as_str(), got[0].url.as_str()),
        ("first", "https://a.example/1.png")
    );
    assert_eq!(
        (got[1].alt.as_str(), got[1].url.as_str()),
        ("", "https://a.example/2.jpg")
    );
}

#[test]
fn span_covers_exactly_the_image_markup() {
    let md = "before ![alt](https://a.example/1.png) after";
    let got = parse_markdown_images(md, PAGE);
    assert_eq!(&md[got[0].span.clone()], "![alt](https://a.example/1.png)");
}

#[test]
fn substack_lazy_blob_yields_the_inner_src() {
    let md = r#"![clone quickjs](<https://simonw.substack.com/p/{"src":"https://substack-post-media.s3.amazonaws.com/public/images/b2a5_1000x1577.jpeg","srcNoWatermark":null,"fullscreen":null,"imageSize":null}>)"#;
    assert_eq!(
        urls(md),
        vec!["https://substack-post-media.s3.amazonaws.com/public/images/b2a5_1000x1577.jpeg"]
    );
}

#[test]
fn percent_encoded_substack_blob_is_decoded() {
    let md =
        "![x](https://h.example/p/%7B%22src%22%3A%22https%3A%2F%2Fsubstack-post-media.s3.amazonaws.com%2Fa.png%22%7D)";
    assert_eq!(urls(md), vec!["https://substack-post-media.s3.amazonaws.com/a.png"]);
}

#[test]
fn relative_urls_resolve_against_the_page() {
    let md = "![a](diagram.png) ![b](/img/b.png) ![c](../c.png) ![d](//cdn.example/d.png)";
    assert_eq!(
        urls(md),
        vec![
            "https://martinfowler.com/articles/diagram.png",
            "https://martinfowler.com/img/b.png",
            "https://martinfowler.com/c.png",
            "https://cdn.example/d.png",
        ]
    );
}

#[test]
fn data_uris_are_kept_as_written_for_the_prefilter_to_count() {
    let md = "![](data:image/gif;base64,R0lGODlh)";
    assert_eq!(urls(md), vec!["data:image/gif;base64,R0lGODlh"]);
}

#[test]
fn relative_url_with_unparseable_page_is_skipped_but_absolute_survives() {
    let got = parse_markdown_images("![a](rel.png) ![b](https://x.example/b.png)", "not a url");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].url, "https://x.example/b.png");
}

#[test]
fn text_without_images_and_plain_links_yield_nothing() {
    assert!(urls("no images [link](https://a.example/x.png) here").is_empty());
    assert!(urls("").is_empty());
    assert!(urls("![unterminated](https://a.example/x.png").is_empty());
}

#[test]
fn link_wrapped_image_is_found() {
    let md = "[![chart](https://a.example/c.png)](https://a.example/full)";
    assert_eq!(urls(md), vec!["https://a.example/c.png"]);
}

#[test]
fn htmd_escaped_parens_are_part_of_the_destination_and_unescaped() {
    let md = r"![diagram](https://example.org/diagram\(v2\).png)";
    let got = parse_markdown_images(md, PAGE);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].url, "https://example.org/diagram(v2).png");
    assert_eq!(&md[got[0].span.clone()], md, "the span ends at the closing paren");
}

#[test]
fn bare_balanced_parens_are_part_of_the_destination() {
    let md = "see ![foo](https://upload.wikimedia.org/wiki/File:Foo_(bar).png \"t\") here";
    let got = parse_markdown_images(md, PAGE);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].url, "https://upload.wikimedia.org/wiki/File:Foo_(bar).png");
    assert_eq!(
        &md[got[0].span.clone()],
        "![foo](https://upload.wikimedia.org/wiki/File:Foo_(bar).png \"t\")"
    );
}

#[test]
fn escaped_parens_inside_angle_brackets_are_unescaped() {
    let md = r"![a](<https://example.org/my diagram\(1\).png>)";
    assert_eq!(urls(md), vec!["https://example.org/my%20diagram(1).png"]);
}

#[test]
fn an_image_in_an_inline_code_span_is_not_a_candidate() {
    let md = "write `![x](https://a.example/code.png)` or ``![y](https://a.example/code2.png)`` then ![real](https://a.example/r.png)";
    assert_eq!(urls(md), vec!["https://a.example/r.png"]);
}

#[test]
fn an_image_in_a_fenced_block_is_not_a_candidate() {
    let md = "intro\n\n```markdown\n![x](https://a.example/code.png)\n```\n\n~~~~\n![y](https://a.example/tilde.png)\n~~~~\n\n![real](https://a.example/r.png)\n";
    assert_eq!(urls(md), vec!["https://a.example/r.png"]);
}

#[test]
fn an_unclosed_fence_runs_to_the_end_and_a_lone_backtick_is_literal() {
    assert!(urls("```\n![x](https://a.example/code.png)\n").is_empty());
    assert_eq!(
        urls("it`s ![real](https://a.example/r.png)"),
        vec!["https://a.example/r.png"]
    );
}
