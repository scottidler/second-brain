//! Intake-side input classification + raw-input sidecar.
//!
//! [`IntakeKind`] is the coarse classification borg assigns at the door
//! (independent of `vault::schema::NoteType` because intake happens before
//! classification and includes kinds - sticker, animation, poll - that never
//! produce a note). The raw-input sidecar (`system/intake/<trace>.txt`) is the
//! durable record of the bytes a trace was received with; it survives even
//! though the legacy `borg-intake.md` table was removed (see
//! docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md). The
//! receipts SQLite DB is the queryable durable record; this module is the
//! kind enum + the sidecar I/O.

use eyre::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Coarse classification of the received input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeKind {
    Url,
    Text,
    Photo,
    Voice,
    Audio,
    Document,
    Sticker,
    Video,
    Animation,
    Poll,
    Location,
    Contact,
    Empty,
    Unknown,
    /// A clyde session/thread candidate pulled by `sb borg harvest`
    /// (harvest-clyde-sessions design). Kept distinct so `intake::
    /// record_received_with_sidecar` can map it to the honest
    /// `ReceiptKind::Session` rather than lying as `Text`.
    Session,
}

impl IntakeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Text => "text",
            Self::Photo => "photo",
            Self::Voice => "voice",
            Self::Audio => "audio",
            Self::Document => "document",
            Self::Sticker => "sticker",
            Self::Video => "video",
            Self::Animation => "animation",
            Self::Poll => "poll",
            Self::Location => "location",
            Self::Contact => "contact",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
            Self::Session => "session",
        }
    }
}

impl std::fmt::Display for IntakeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for IntakeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "url" => Ok(Self::Url),
            "text" => Ok(Self::Text),
            "photo" => Ok(Self::Photo),
            "voice" => Ok(Self::Voice),
            "audio" => Ok(Self::Audio),
            "document" => Ok(Self::Document),
            "sticker" => Ok(Self::Sticker),
            "video" => Ok(Self::Video),
            "animation" => Ok(Self::Animation),
            "poll" => Ok(Self::Poll),
            "location" => Ok(Self::Location),
            "contact" => Ok(Self::Contact),
            "empty" => Ok(Self::Empty),
            "unknown" => Ok(Self::Unknown),
            "session" => Ok(Self::Session),
            _ => Err(format!("unknown intake kind: {s}")),
        }
    }
}

/// Resolve the directory where raw-input sidecar files live.
pub fn intake_raw_dir(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("intake")
}

/// Path for a single trace's raw-input sidecar file.
pub fn raw_input_path(vault_root: &Path, trace_id: &str) -> PathBuf {
    intake_raw_dir(vault_root).join(format!("{trace_id}.txt"))
}

/// Write the raw-input sidecar for a trace. Bytes are written verbatim; for
/// large binary inputs the caller is expected to pass a short descriptor
/// (e.g. `[image: foo.jpg, 12345 bytes, image/jpeg]`) rather than the raw
/// payload, to keep `system/intake/` small.
pub fn write_raw_input(vault_root: &Path, trace_id: &str, bytes: &[u8]) -> Result<()> {
    log::debug!("intake::write_raw_input: trace={trace_id} bytes={}", bytes.len());
    let dir = intake_raw_dir(vault_root);
    fs::create_dir_all(&dir).context("Failed to create intake raw-input directory")?;
    let path = raw_input_path(vault_root, trace_id);
    fs::write(&path, bytes).with_context(|| format!("Failed to write raw-input sidecar {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
