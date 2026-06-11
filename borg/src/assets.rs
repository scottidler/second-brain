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
mod tests;
