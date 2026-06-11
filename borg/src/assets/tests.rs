use super::*;
use std::fs;

#[test]
fn test_store_asset_creates_file() {
    let tmp = std::env::temp_dir().join("obsidian-borg-test-assets-create");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("create tmp");

    let data = b"fake image data";
    let (abs_path, rel_path) = store_asset(&tmp, data, "photo.png", "images/2026-03").expect("store");

    assert!(abs_path.exists(), "File should exist at {}", abs_path.display());
    assert_eq!(fs::read(&abs_path).expect("read"), data);
    assert!(rel_path.starts_with("system/attachments/images/2026-03/"));
    assert!(rel_path.ends_with(".png"));
    assert!(rel_path.contains("-")); // has hash suffix

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_store_asset_hash_suffix_uniqueness() {
    let tmp = std::env::temp_dir().join("obsidian-borg-test-assets-hash");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("create tmp");

    let (_, rel1) = store_asset(&tmp, b"data1", "photo.png", "images").expect("store1");
    let (_, rel2) = store_asset(&tmp, b"data2", "photo.png", "images").expect("store2");

    assert_ne!(rel1, rel2, "Different data should produce different filenames");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_store_asset_creates_directories() {
    let tmp = std::env::temp_dir().join("obsidian-borg-test-assets-dirs");
    let _ = fs::remove_dir_all(&tmp);
    // Don't create tmp - let store_asset create everything

    let (abs_path, _) = store_asset(&tmp, b"test", "file.jpg", "images/2026-03").expect("store");
    assert!(abs_path.exists());

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_store_asset_sanitizes_filename() {
    let tmp = std::env::temp_dir().join("obsidian-borg-test-assets-sanitize");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("create tmp");

    let (_, rel_path) = store_asset(&tmp, b"test", "My Photo (1).png", "images").expect("store");
    // The filename portion should be lowercase-hyphenated, no spaces or parens
    let filename_part = rel_path.rsplit('/').next().expect("has filename");
    assert!(
        !filename_part.contains(' '),
        "filename should not contain spaces: {filename_part}"
    );
    assert!(
        !filename_part.contains('('),
        "filename should not contain parens: {filename_part}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn test_is_image_extension() {
    assert!(is_image_extension("photo.png"));
    assert!(is_image_extension("image.JPG"));
    assert!(is_image_extension("test.jpeg"));
    assert!(is_image_extension("pic.gif"));
    assert!(is_image_extension("web.webp"));
    assert!(is_image_extension("draw.svg"));
    assert!(is_image_extension("scan.bmp"));
    assert!(is_image_extension("high.tiff"));
    assert!(!is_image_extension("doc.pdf"));
    assert!(!is_image_extension("music.mp3"));
    assert!(!is_image_extension("noext"));
}

#[test]
fn test_is_pdf_extension() {
    assert!(is_pdf_extension("report.pdf"));
    assert!(is_pdf_extension("DOCUMENT.PDF"));
    assert!(is_pdf_extension("my-file.Pdf"));
    assert!(!is_pdf_extension("image.png"));
    assert!(!is_pdf_extension("doc.docx"));
    assert!(!is_pdf_extension("noext"));
}

#[test]
fn test_is_document_extension() {
    assert!(is_document_extension("report.docx"));
    assert!(is_document_extension("slides.pptx"));
    assert!(is_document_extension("data.xlsx"));
    assert!(is_document_extension("book.epub"));
    assert!(is_document_extension("text.odt"));
    assert!(is_document_extension("legacy.rtf"));
    assert!(is_document_extension("REPORT.DOCX"));
    assert!(!is_document_extension("report.pdf"));
    assert!(!is_document_extension("image.png"));
    assert!(!is_document_extension("noext"));
}

#[test]
fn test_is_audio_extension() {
    assert!(is_audio_extension("song.mp3"));
    assert!(is_audio_extension("recording.wav"));
    assert!(is_audio_extension("voice.ogg"));
    assert!(is_audio_extension("memo.opus"));
    assert!(is_audio_extension("track.m4a"));
    assert!(is_audio_extension("lossless.flac"));
    assert!(is_audio_extension("clip.aac"));
    assert!(is_audio_extension("old.wma"));
    assert!(is_audio_extension("stream.webm"));
    assert!(is_audio_extension("RECORDING.MP3"));
    assert!(is_audio_extension("Voice.OGG"));
    assert!(!is_audio_extension("image.png"));
    assert!(!is_audio_extension("doc.pdf"));
    assert!(!is_audio_extension("noext"));
}
