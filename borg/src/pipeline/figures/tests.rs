use super::*;
use chrono::TimeZone;
use image::{ImageBuffer, Rgb};

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap()
}

/// Deterministic noise: incompressible, so the encoded size tracks the pixel count.
fn noisy(width: u32, height: u32) -> image::DynamicImage {
    let mut state: u32 = 0x9e37_79b9;
    let buf = ImageBuffer::from_fn(width, height, |_, _| {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let [a, b, c, _] = state.to_be_bytes();
        Rgb([a, b, c])
    });
    image::DynamicImage::ImageRgb8(buf)
}

fn encode(img: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn figure(bytes: Vec<u8>, mime: FigureMime) -> Figure {
    Figure {
        bytes,
        mime,
        section: "#### Description\n\nd".to_string(),
    }
}

fn set(figures: Vec<Figure>) -> FigureSet {
    FigureSet {
        figures,
        degraded: false,
    }
}

fn dims(bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory(bytes).unwrap();
    (img.width(), img.height())
}

#[test]
fn plan_names_each_figure_by_trace_position_hash_and_mime_extension() {
    let bytes = b"not really a png".to_vec();
    let plan = plan(
        &set(vec![
            figure(bytes.clone(), FigureMime::Png),
            figure(b"<svg/>".to_vec(), FigureMime::Svg),
        ]),
        "cl-8c531660",
        &now(),
        &ImagesConfig::default(),
    );
    assert!(!plan.degraded);
    assert_eq!(plan.planned.len(), 2);
    let first = &plan.planned[0];
    let hash = sha8(&bytes);
    assert_eq!(hash.len(), 8);
    assert_eq!(first.basename, format!("cl-8c531660-1-{hash}.png"));
    assert_eq!(
        first.vault_path,
        format!("system/attachments/images/2026-10/cl-8c531660-1-{hash}.png")
    );
    assert_eq!(first.bytes, bytes);
    assert!(plan.planned[1].basename.starts_with("cl-8c531660-2-"));
    assert!(plan.planned[1].basename.ends_with(".svg"));
}

#[test]
fn plan_month_comes_from_the_ingest_time() {
    let december = Utc.with_ymd_and_hms(2026, 12, 31, 23, 0, 0).unwrap();
    let plan = plan(
        &set(vec![figure(vec![1, 2, 3], FigureMime::Gif)]),
        "t",
        &december,
        &ImagesConfig::default(),
    );
    assert!(plan.planned[0].vault_path.contains("/images/2026-12/"));
}

#[test]
fn plan_downscales_an_oversize_png_to_max_side() {
    let big = encode(&noisy(200, 100), image::ImageFormat::Png);
    let cfg = ImagesConfig {
        max_bytes: big.len() as u64 / 2,
        max_side_px: 60,
        ..ImagesConfig::default()
    };
    let plan = plan(&set(vec![figure(big.clone(), FigureMime::Png)]), "t", &now(), &cfg);
    assert!(!plan.degraded, "a 60px noise png fits under half the 200px size");
    let stored = &plan.planned[0];
    assert!(stored.bytes.len() < big.len());
    assert!(stored.bytes.len() as u64 <= cfg.max_bytes);
    assert_eq!(dims(&stored.bytes), (60, 30));
    assert!(stored.basename.ends_with(".png"));
}

#[test]
fn plan_keeps_jpeg_a_jpeg_and_turns_gif_into_png() {
    let img = noisy(200, 100);
    let jpeg = encode(&img, image::ImageFormat::Jpeg);
    let gif = encode(&img, image::ImageFormat::Gif);
    let cfg = ImagesConfig {
        max_bytes: (jpeg.len().min(gif.len()) as u64) - 1,
        max_side_px: 40,
        ..ImagesConfig::default()
    };
    let jpeg_plan = plan(&set(vec![figure(jpeg, FigureMime::Jpeg)]), "t", &now(), &cfg);
    assert!(jpeg_plan.planned[0].basename.ends_with(".jpg"));
    assert!(image::guess_format(&jpeg_plan.planned[0].bytes).unwrap() == image::ImageFormat::Jpeg);
    let gif_plan = plan(&set(vec![figure(gif, FigureMime::Gif)]), "t", &now(), &cfg);
    assert!(gif_plan.planned[0].basename.ends_with(".png"));
    assert!(image::guess_format(&gif_plan.planned[0].bytes).unwrap() == image::ImageFormat::Png);
}

#[test]
fn plan_drops_a_figure_still_over_max_bytes_and_degrades_without_renumbering() {
    let big = encode(&noisy(120, 120), image::ImageFormat::Png);
    let cfg = ImagesConfig {
        max_bytes: 100,
        max_side_px: 100,
        ..ImagesConfig::default()
    };
    let plan = plan(
        &set(vec![figure(big, FigureMime::Png), figure(vec![9; 50], FigureMime::Svg)]),
        "t",
        &now(),
        &cfg,
    );
    assert!(plan.degraded);
    assert_eq!(plan.planned.len(), 1);
    assert!(
        plan.planned[0].basename.starts_with("t-2-"),
        "position 2 survives a drop of position 1"
    );
}

#[test]
fn plan_drops_an_undecodable_oversize_figure() {
    let cfg = ImagesConfig {
        max_bytes: 10,
        ..ImagesConfig::default()
    };
    let plan = plan(&set(vec![figure(vec![0xAB; 64], FigureMime::Png)]), "t", &now(), &cfg);
    assert!(plan.degraded);
    assert!(plan.planned.is_empty());
}

#[test]
fn plan_never_resizes_an_svg() {
    let svg = vec![b'x'; 5000];
    let cfg = ImagesConfig {
        max_bytes: 10,
        ..ImagesConfig::default()
    };
    let plan = plan(&set(vec![figure(svg.clone(), FigureMime::Svg)]), "t", &now(), &cfg);
    assert!(!plan.degraded);
    assert_eq!(plan.planned[0].bytes, svg);
}

fn planned(path: &str, bytes: &[u8]) -> PlannedFigure {
    PlannedFigure {
        vault_path: path.to_string(),
        basename: path.rsplit('/').next().unwrap().to_string(),
        bytes: bytes.to_vec(),
    }
}

#[test]
fn store_writes_files_and_returns_what_it_created() {
    let vault = tempfile::tempdir().unwrap();
    let figures = [
        planned("system/attachments/images/2026-10/t-1-aaaaaaaa.png", b"one"),
        planned("system/attachments/images/2026-10/t-2-bbbbbbbb.png", b"two"),
    ];
    let created = store(vault.path(), &figures).unwrap();
    assert_eq!(created.len(), 2);
    assert_eq!(std::fs::read(&created[0]).unwrap(), b"one");
    assert_eq!(std::fs::read(&created[1]).unwrap(), b"two");
}

#[test]
fn store_never_overwrites_and_does_not_claim_a_reused_path() {
    let vault = tempfile::tempdir().unwrap();
    let dir = vault.path().join("system/attachments/images/2026-10");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t-1-aaaaaaaa.png"), b"original").unwrap();
    let figures = [
        planned("system/attachments/images/2026-10/t-1-aaaaaaaa.png", b"clobber"),
        planned("system/attachments/images/2026-10/t-2-bbbbbbbb.png", b"new"),
    ];
    let created = store(vault.path(), &figures).unwrap();
    assert_eq!(created, vec![dir.join("t-2-bbbbbbbb.png")]);
    assert_eq!(std::fs::read(dir.join("t-1-aaaaaaaa.png")).unwrap(), b"original");
}

#[test]
fn store_failure_removes_what_it_already_created() {
    let vault = tempfile::tempdir().unwrap();
    // `blocker` is a file, so a figure under it cannot create its parent dir.
    std::fs::write(vault.path().join("blocker"), b"file").unwrap();
    let figures = [
        planned("system/attachments/images/2026-10/t-1-aaaaaaaa.png", b"one"),
        planned("blocker/t-2-bbbbbbbb.png", b"two"),
    ];
    let err = store(vault.path(), &figures).unwrap_err();
    assert!(format!("{err:#}").contains("t-2-bbbbbbbb.png"), "{err:#}");
    assert!(!vault.path().join(&figures[0].vault_path).exists());
}
