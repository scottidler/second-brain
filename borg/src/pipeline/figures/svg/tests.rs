use super::*;

fn decode(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).expect("decode raster").to_rgba8()
}

fn count(img: &image::RgbaImage, f: impl Fn(&[u8; 4]) -> bool) -> usize {
    img.pixels().filter(|p| f(&p.0)).count()
}

fn is_red(p: &[u8; 4]) -> bool {
    p[0] > 200 && p[1] < 60 && p[2] < 60
}

fn is_not_white(p: &[u8; 4]) -> bool {
    p[0] < 250 || p[1] < 250 || p[2] < 250
}

/// A red 20x20 PNG on disk, and an SVG that embeds it by absolute path.
fn svg_embedding_local_png(dir: &std::path::Path) -> String {
    let png_path = dir.join("secret.png");
    image::RgbImage::from_pixel(20, 20, image::Rgb([255, 0, 0]))
        .save(&png_path)
        .expect("write fixture");
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="40" viewBox="0 0 40 40"><image x="0" y="0" width="40" height="40" xlink:href="{}"/></svg>"#,
        png_path.display()
    )
}

#[test]
fn locked_resolver_renders_a_local_file_href_absent_where_the_default_reads_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let svg = svg_embedding_local_png(dir.path());

    let default = rasterize_with(svg.as_bytes(), 2048, &usvg::Options::default()).expect("default render");
    assert!(
        count(&decode(&default.png), is_red) > 100,
        "precondition: the default resolver must read the fixture, or this test proves nothing"
    );

    let locked = rasterize(svg.as_bytes(), 2048).expect("locked render");
    assert_eq!(
        count(&decode(&locked.png), is_red),
        0,
        "the locked resolver must not read a local file into the raster"
    );
}

#[test]
fn locked_resolver_refuses_a_data_uri_href_too() {
    let red = {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(20, 20, image::Rgb([255, 0, 0]))
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode");
        out.into_inner()
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(red);
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="40" height="40"><image width="40" height="40" xlink:href="data:image/png;base64,{b64}"/></svg>"#
    );
    let default = rasterize_with(svg.as_bytes(), 2048, &usvg::Options::default()).expect("default");
    assert!(count(&decode(&default.png), is_red) > 100, "precondition");
    let locked = rasterize(svg.as_bytes(), 2048).expect("locked");
    assert_eq!(count(&decode(&locked.png), is_red), 0);
}

#[test]
fn locked_options_set_no_resources_dir() {
    assert!(locked_options().resources_dir.is_none());
}

#[test]
fn a_huge_viewbox_rasterizes_within_max_side_on_the_longer_side() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100000 50000"><rect width="100000" height="50000" fill="blue"/></svg>"#;
    let raster = rasterize(svg, 512).expect("render");
    let img = decode(&raster.png);
    assert!(img.width().max(img.height()) <= 512, "{}x{}", img.width(), img.height());
    assert_eq!((img.width(), img.height()), (512, 256));
    assert_eq!(raster.intrinsic, (100_000, 50_000));
}

#[test]
fn a_small_svg_is_raised_to_a_legible_raster() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="blue"/></svg>"#;
    let raster = rasterize(svg, 2048).expect("render");
    let img = decode(&raster.png);
    assert_eq!((img.width(), img.height()), (1024, 512));
    assert_eq!(raster.intrinsic, (100, 50));
}

#[test]
fn text_in_an_unknown_webfont_still_draws_pixels() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="100"><text x="10" y="70" font-family="NoSuchWebfont" font-size="64" fill="black">Hello</text></svg>"#;
    let raster = rasterize(svg, 2048).expect("render");
    assert!(
        count(&decode(&raster.png), is_not_white) > 200,
        "a text node must produce non-background pixels"
    );
}

#[test]
fn malformed_svg_is_an_error() {
    assert!(rasterize(b"<svg", 2048).is_err());
    assert!(rasterize(b"", 2048).is_err());
}
