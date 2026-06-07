use super::*;

#[test]
fn rung1_meta_name_author() {
    let html = r#"<html><head><meta name="author" content="Jane Doe"></head></html>"#;
    assert_eq!(extract(html), Some("Jane Doe".to_string()));
}

#[test]
fn rung1_meta_name_author_attr_order_and_case() {
    // content before name, mixed case, single quotes.
    let html = r#"<META CONTENT='Ada Lovelace' NAME='Author'>"#;
    assert_eq!(extract(html), Some("Ada Lovelace".to_string()));
}

#[test]
fn rung2_jsonld_author_string() {
    let html = r#"<script type="application/ld+json">{"@type":"Article","author":"Carl Sagan"}</script>"#;
    assert_eq!(extract(html), Some("Carl Sagan".to_string()));
}

#[test]
fn rung2_jsonld_author_object_with_name() {
    let html = r#"<script type="application/ld+json">
        {"@type":"Article","author":{"@type":"Person","name":"Grace Hopper"}}
    </script>"#;
    assert_eq!(extract(html), Some("Grace Hopper".to_string()));
}

#[test]
fn rung2_jsonld_author_array_takes_first() {
    let html = r#"<script type="application/ld+json">
        {"author":[{"name":"First Author"},{"name":"Second Author"}]}
    </script>"#;
    assert_eq!(extract(html), Some("First Author".to_string()));
}

#[test]
fn rung2_jsonld_author_array_of_strings_takes_first() {
    let html = r#"<script type="application/ld+json">{"author":["Alpha","Beta"]}</script>"#;
    assert_eq!(extract(html), Some("Alpha".to_string()));
}

#[test]
fn rung2_jsonld_author_in_graph() {
    let html = r#"<script type="application/ld+json">
        {"@context":"https://schema.org","@graph":[
            {"@type":"WebSite"},
            {"@type":"Article","author":{"name":"Graph Author"}}
        ]}
    </script>"#;
    assert_eq!(extract(html), Some("Graph Author".to_string()));
}

#[test]
fn rung3_meta_article_author() {
    let html = r#"<meta property="article:author" content="Byline Person">"#;
    assert_eq!(extract(html), Some("Byline Person".to_string()));
}

#[test]
fn rung3_meta_og_article_author() {
    let html = r#"<meta property="og:article:author" content="OG Person">"#;
    assert_eq!(extract(html), Some("OG Person".to_string()));
}

#[test]
fn rung4_a_rel_author_text() {
    let html = r#"<p>By <a rel="author" href="/u/kp">Katherine Johnson</a></p>"#;
    assert_eq!(extract(html), Some("Katherine Johnson".to_string()));
}

#[test]
fn rung4_a_rel_author_strips_inner_tags() {
    let html = r#"<a rel="author"><span>Margaret</span> Hamilton</a>"#;
    assert_eq!(extract(html), Some("Margaret Hamilton".to_string()));
}

#[test]
fn ladder_precedence_meta_name_beats_jsonld() {
    let html = r#"
        <meta name="author" content="Meta Wins">
        <script type="application/ld+json">{"author":"JsonLd Loses"}</script>
    "#;
    assert_eq!(extract(html), Some("Meta Wins".to_string()));
}

#[test]
fn no_author_returns_none() {
    let html = r#"<html><head><title>No byline here</title></head><body><p>Text.</p></body></html>"#;
    assert_eq!(extract(html), None);
}

#[test]
fn empty_html_returns_none() {
    assert_eq!(extract(""), None);
}

#[test]
fn entities_are_decoded() {
    let html = r#"<meta name="author" content="Ben &amp; Jerry">"#;
    assert_eq!(extract(html), Some("Ben & Jerry".to_string()));
}

#[test]
fn whitespace_is_collapsed_and_trimmed() {
    let html = "<meta name=\"author\" content=\"  Jane\n\t  Doe  \">";
    assert_eq!(extract(html), Some("Jane Doe".to_string()));
}

#[test]
fn empty_content_falls_through_to_none() {
    let html = r#"<meta name="author" content="">"#;
    assert_eq!(extract(html), None);
}

#[test]
fn pathologically_long_value_is_rejected() {
    let long = "x".repeat(MAX_AUTHOR_LEN + 1);
    let html = format!(r#"<meta name="author" content="{long}">"#);
    assert_eq!(extract(&html), None);
}

#[test]
fn malformed_jsonld_is_skipped_not_panicked() {
    // First block is invalid JSON; second is valid - extraction recovers.
    let html = r#"
        <script type="application/ld+json">{ not valid json ,, }</script>
        <script type="application/ld+json">{"author":"Recovered"}</script>
    "#;
    assert_eq!(extract(html), Some("Recovered".to_string()));
}

#[test]
fn name_substring_does_not_false_match() {
    // `nickname` contains `name` but is not the `name` attribute; must not
    // be read as author.
    let html = r#"<meta nickname="author" content="Should Not Match"><p>body</p>"#;
    assert_eq!(extract(html), None);
}
