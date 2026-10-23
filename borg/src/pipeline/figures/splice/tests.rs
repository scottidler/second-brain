use super::*;
use crate::pipeline::figures::candidates::parse_markdown_images;

const PAGE: &str = "https://example.com/post";

/// Splice a block for every image whose url contains one of `keep`.
fn spliced(md: &str, keep: &[&str]) -> String {
    let images = parse_markdown_images(md, PAGE);
    let placed: Vec<(&MarkdownImage, String)> = images
        .iter()
        .filter(|i| keep.iter().any(|k| i.url.contains(k)))
        .enumerate()
        .map(|(n, i)| {
            (
                i,
                figure_block(n + 1, &i.alt, &format!("#### Description\n\n{} desc", i.url)),
            )
        })
        .collect();
    splice(md, &placed)
}

#[test]
fn an_image_on_its_own_line_is_replaced_by_its_block() {
    let md = "Intro.\n\n![The flow](https://example.com/a.png)\n\nOutro.";
    assert_eq!(
        spliced(md, &["a.png"]),
        "Intro.\n\n### Image 1\n\n*The flow*\n\n#### Description\n\nhttps://example.com/a.png desc\n\nOutro."
    );
}

#[test]
fn a_linked_image_line_is_replaced_whole() {
    let md = "Intro.\n[![](https://example.com/a.png)](https://example.com/full.png)\nOutro.";
    let out = spliced(md, &["a.png"]);
    assert_eq!(
        out,
        "Intro.\n### Image 1\n\n#### Description\n\nhttps://example.com/a.png desc\nOutro."
    );
}

#[test]
fn an_image_inside_prose_keeps_the_prose_around_its_block() {
    let md = "See ![x](https://example.com/a.png) for the shape.";
    let out = spliced(md, &["a.png"]);
    assert!(out.starts_with("See \n\n### Image 1\n\n*x*\n\n"), "{out}");
    assert!(out.ends_with("desc\n\n for the shape."), "{out}");
}

#[test]
fn unkept_images_stay_as_written_and_kept_ones_are_numbered_in_order() {
    let md = "![a](https://example.com/a.png)\n![b](https://example.com/b.png)\n![c](https://example.com/c.png)\n";
    let out = spliced(md, &["a.png", "c.png"]);
    assert!(out.contains("![b](https://example.com/b.png)"), "{out}");
    let one = out.find("### Image 1\n\n*a*").expect("a placed");
    let two = out.find("### Image 2\n\n*c*").expect("c placed");
    let b = out.find("![b]").expect("b kept");
    assert!(one < b && b < two, "{out}");
    assert!(!out.contains("![a]") && !out.contains("![c]"), "{out}");
}

#[test]
fn two_images_on_one_line_are_both_spliced() {
    let md = "![a](https://example.com/a.png) ![b](https://example.com/b.png)";
    let out = spliced(md, &["a.png", "b.png"]);
    assert!(out.find("*a*").unwrap() < out.find("*b*").unwrap(), "{out}");
    assert!(!out.contains("!["), "{out}");
}

#[test]
fn nothing_placed_returns_the_markdown_unchanged() {
    let md = "![a](https://example.com/a.png)\n";
    assert_eq!(splice(md, &[]), md);
}

#[test]
fn an_escaped_paren_url_is_spliced_whole_with_no_residue() {
    let md = "before\n\n![diagram](https://example.org/diagram\\(v2\\).png)\n\nafter ![w](https://upload.wikimedia.org/File:Foo_(bar).png) end\n";
    let images = parse_markdown_images(md, PAGE);
    assert_eq!(images.len(), 2);
    let placed = [(&images[0], "BLOCK-1".to_string()), (&images[1], "BLOCK-2".to_string())];
    let out = splice(md, &placed);
    assert!(!out.contains(".png)"), "{out}");
    assert!(!out.contains("v2"), "{out}");
    assert_eq!(out, "before\n\nBLOCK-1\n\nafter \n\nBLOCK-2\n\n end\n");
}
