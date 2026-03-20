use eyre::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::hygiene;

/// Store a binary asset in the vault's attachment directory.
/// Returns (absolute_path, vault-relative path) for frontmatter.
pub fn store_asset(
    vault_root: &Path,
    data: &[u8],
    filename: &str,
    subdirectory: &str, // e.g. "images/2026-03", "pdfs"
) -> Result<(PathBuf, String)> {
    let sanitized = hygiene::sanitize_filename(filename);

    // Compute content hash (first 8 hex chars of SHA-256)
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = format!("{:x}", hasher.finalize());
    let hash_prefix = &hash[..8];

    // Split sanitized name into stem and extension
    let (stem, ext) = if let Some(dot_pos) = filename.rfind('.') {
        let ext = &filename[dot_pos..]; // includes the dot
        let stem = hygiene::sanitize_filename(&filename[..dot_pos]);
        (stem, ext.to_lowercase())
    } else {
        (sanitized, String::new())
    };

    let unique_filename = format!("{stem}-{hash_prefix}{ext}");

    let attachments_dir = vault_root.join("system/attachments").join(subdirectory);
    std::fs::create_dir_all(&attachments_dir).context(format!(
        "Failed to create attachment directory: {}",
        attachments_dir.display()
    ))?;

    let absolute_path = attachments_dir.join(&unique_filename);
    std::fs::write(&absolute_path, data).context("Failed to write asset file")?;

    let relative_path = format!("system/attachments/{subdirectory}/{unique_filename}");

    Ok((absolute_path, relative_path))
}

/// Known image extensions.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tiff"];

/// Known PDF extensions.
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

/// Known document extensions.
pub const DOCUMENT_EXTENSIONS: &[&str] = &["docx", "pptx", "xlsx", "epub", "odt", "rtf"];

/// Known audio extensions.
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "ogg", "opus", "m4a", "flac", "aac", "wma", "webm"];

/// Check if a filename has an image extension.
pub fn is_image_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Check if a filename has a PDF extension.
pub fn is_pdf_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    PDF_EXTENSIONS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Check if a filename has a document extension.
pub fn is_document_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    DOCUMENT_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Check if a filename has an audio extension.
pub fn is_audio_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    AUDIO_EXTENSIONS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

#[cfg(test)]
mod tests {
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
}
