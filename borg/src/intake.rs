//! Borg-side durable-capture entry points.
//!
//! Every door (telegram, http, ntfy, discord, signal, cli) generates a
//! `trace_id` and calls [`record_received_with_sidecar`] BEFORE any
//! classification, filter, or pipeline dispatch: it writes the raw-input
//! sidecar and the `received` receipts row, both propagating, so no accepted
//! input is ever silently dropped. A rejection or fetch-fail at the door calls
//! [`record_failure_at_door`] with the per-site `FailureStage`. The legacy
//! markdown intake/DLQ tables were removed (see
//! docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md); the
//! receipts SQLite DB is the sole durable store.

use crate::config::Config;
use crate::receipts;
use crate::types::IngestMethod;
use eyre::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use vault::intake::{self, IntakeKind};
use vault::receipts::{FailureStage, ReceiptKind};

/// Map the intake-side kind (rich classification, includes media subtypes) to
/// the receipts-side kind (flat `url`/`text`/`binary`).
fn receipt_kind(kind: IntakeKind) -> ReceiptKind {
    match kind {
        IntakeKind::Url => ReceiptKind::Url,
        IntakeKind::Text | IntakeKind::Empty | IntakeKind::Unknown => ReceiptKind::Text,
        IntakeKind::Photo
        | IntakeKind::Voice
        | IntakeKind::Audio
        | IntakeKind::Document
        | IntakeKind::Sticker
        | IntakeKind::Video
        | IntakeKind::Animation
        | IntakeKind::Poll
        | IntakeKind::Location
        | IntakeKind::Contact => ReceiptKind::Binary,
    }
}

/// Resolve the vault root from borg config via the unified resolver.
pub fn vault_root(config: &Config) -> Result<PathBuf> {
    config.vault_root()
}

/// Truncate `s` to 80 characters (preview budget), appending `...` if cut.
pub fn preview_text(s: &str) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.len() <= 80 { one_line } else { format!("{}...", &one_line[..80]) }
}

/// Build a structured descriptor for a binary input. Used as the preview
/// (and raw-input sidecar contents) for photos, voice notes, audio,
/// documents, stickers, etc.
pub fn binary_descriptor(kind: IntakeKind, filename: &str, bytes_len: usize, mime: Option<&str>) -> String {
    let kind_label = kind.as_str();
    match mime {
        Some(m) => format!("[{kind_label}: {filename}, {bytes_len} bytes, {m}]"),
        None => format!("[{kind_label}: {filename}, {bytes_len} bytes]"),
    }
}

/// Capture an input at the door: write the raw-input sidecar AND the
/// `received` receipts row. BOTH writes propagate - on any failure the caller
/// (the door) surfaces `Failed` and does not dispatch. This is the
/// immediate-capture invariant. The sidecar payload is unchanged from the
/// legacy path: verbatim text/URL, or a short descriptor for large binaries
/// (the remote doors do not hold the raw bytes at this checkpoint).
///
/// Replaces `record_intake` / `record_intake_with_sidecar` (markdown +
/// sidecar + best-effort receipts) once the call sites are switched in Phase 2.
pub fn record_received_with_sidecar(
    config: &Config,
    method: IngestMethod,
    kind: IntakeKind,
    preview: &str,
    sidecar_bytes: &[u8],
    trace_id: &str,
) -> Result<()> {
    log::debug!(
        "intake::record_received_with_sidecar: trace={trace_id} method={method} kind={kind} sidecar_bytes={}",
        sidecar_bytes.len()
    );
    let conn = receipts::open_default().context("open receipts DB for capture")?;
    let root = vault_root(config)?;
    record_received_with_sidecar_to(&conn, &root, method, kind, preview, sidecar_bytes, trace_id)
}

/// Conn/root-injectable core of [`record_received_with_sidecar`]. Sidecar
/// first (raw-input record lands on disk), then the receipts row. Both
/// propagate.
fn record_received_with_sidecar_to(
    conn: &Connection,
    vault_root: &Path,
    method: IngestMethod,
    kind: IntakeKind,
    preview: &str,
    sidecar_bytes: &[u8],
    trace_id: &str,
) -> Result<()> {
    intake::write_raw_input(vault_root, trace_id, sidecar_bytes).context("Failed to write intake sidecar")?;
    receipts::record_received(conn, trace_id, method.into(), receipt_kind(kind), preview)
        .with_context(|| format!("Failed to record receipts row trace={trace_id}"))?;
    log::info!(
        "intake: captured trace={trace_id} method={method} kind={kind} preview={}",
        preview_text(preview)
    );
    Ok(())
}

/// Record a terminal failure at the door (rejection or fetch-fail), replacing
/// `record_dlq`. Carries the per-site `FailureStage` so Signal's `FetchFailed`
/// is not collapsed to `IntakeRejected`. Best-effort: the input's durability
/// is already guaranteed by the preceding [`record_received_with_sidecar`]; a
/// failed `mark_failed` leaves the row `received` for the watchdog to
/// crash-promote, so we log and move on.
pub fn record_failure_at_door(method: IngestMethod, trace_id: &str, stage: FailureStage, reason: &str) {
    log::debug!("intake::record_failure_at_door: trace={trace_id} method={method} stage={stage} reason={reason}");
    let conn = match receipts::open_default() {
        Ok(c) => c,
        Err(e) => {
            log::error!("receipts: failed to open DB for failure trace={trace_id}: {e:#}");
            return;
        }
    };
    if let Err(e) = record_failure_at_door_to(&conn, method, trace_id, stage, reason) {
        log::error!("receipts: failed to record failure trace={trace_id} stage={stage}: {e:#}");
    }
}

/// Conn-injectable core of [`record_failure_at_door`]. UPSERT (gap-proof):
/// INSERT-OR-IGNORE a `received` row (a no-op preserving the real captured
/// data when the preceding capture already ran; a cold-path insert otherwise),
/// then `mark_failed` with the given stage. A rejection therefore lands a
/// `failed` row regardless of whether a prior capture ran in this control flow.
fn record_failure_at_door_to(
    conn: &Connection,
    method: IngestMethod,
    trace_id: &str,
    stage: FailureStage,
    reason: &str,
) -> Result<()> {
    // Cold-path values (kind=Text, raw_input=reason) apply only if no row
    // exists yet; INSERT OR IGNORE never clobbers a prior capture's data.
    receipts::record_received(conn, trace_id, method.into(), ReceiptKind::Text, reason)
        .with_context(|| format!("upsert received row for failure trace={trace_id}"))?;
    receipts::mark_failed(conn, trace_id, stage, reason)
        .with_context(|| format!("mark_failed trace={trace_id} stage={stage}"))?;
    Ok(())
}

// Re-export the intake-kind enum so call sites can `use crate::intake::Kind`.
pub use vault::intake::IntakeKind as Kind;

#[cfg(test)]
mod tests;
