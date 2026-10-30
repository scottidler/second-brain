//! Per-kind distance offsets for `SearchIndex::search_vector`'s min-reduce.

use serde::Deserialize;

/// A distance added to every row of a kind before the per-note min-reduce, so
/// an operator can make one representation (say a transcript chunk) win a note
/// less often than its summary. Positive pushes a kind away; all zero is the
/// plain min-reduce, bit for bit.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case", default, deny_unknown_fields)]
pub struct KindOffsets {
    pub summary: f32,
    pub transcript_chunk: f32,
    pub claim: f32,
}

impl Default for KindOffsets {
    fn default() -> Self {
        Self {
            summary: 0.0,
            transcript_chunk: 0.0,
            claim: 0.0,
        }
    }
}

impl KindOffsets {
    /// The offset for a `note_embeddings.kind` column value. An unknown kind
    /// is an error: the column's CHECK constraint makes it unreachable, so
    /// reaching it means the schema and this table drifted.
    pub fn for_kind(&self, kind: &str) -> eyre::Result<f32> {
        match kind {
            "summary" => Ok(self.summary),
            "transcript-chunk" => Ok(self.transcript_chunk),
            "claim" => Ok(self.claim),
            other => Err(eyre::eyre!("note_embeddings row carries unknown kind {other:?}")),
        }
    }

    /// Rejects a NaN or infinite offset, which would poison every distance.
    pub fn validate(&self) -> eyre::Result<()> {
        for (name, v) in [
            ("summary", self.summary),
            ("transcript-chunk", self.transcript_chunk),
            ("claim", self.claim),
        ] {
            if !v.is_finite() {
                eyre::bail!("kind-offsets.{name} must be finite, got {v}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
