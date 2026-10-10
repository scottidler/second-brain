//! Which embed kind leads the next daemon tick.
//!
//! The tick allowance is shared by every kind in run order, summary first. A
//! later kind whose next note does not fit what the earlier kinds left over
//! waits for the next tick. On its own that wait can last forever: a summary
//! that fails inference every tick still draws from the allowance and stays
//! stale, so the transcript kind never sees a fresh allowance and its
//! oversized note (or any note larger than what the summary leaves) never
//! fits. The daemon keeps one [`EmbedCarry`] across ticks, and the kind that
//! was left waiting with nothing embedded runs first, on a fresh allowance,
//! in the next tick.

use vault::search::EmbeddingKind;

/// Daemon-held state carried from one embed tick to the next.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EmbedCarry {
    lead: Option<EmbeddingKind>,
}

impl EmbedCarry {
    /// The kind that leads the next tick, if one was left waiting.
    pub fn lead(&self) -> Option<EmbeddingKind> {
        self.lead
    }

    /// `kinds` rotated so the carried kind runs first. The rotation keeps the
    /// rest in their configured cycle, so the kinds take turns when more than
    /// one is left waiting. A carried kind that is not enabled changes nothing.
    pub(super) fn run_order(&self, mut kinds: Vec<EmbeddingKind>) -> Vec<EmbeddingKind> {
        if let Some(pos) = self.lead.and_then(|lead| kinds.iter().position(|k| *k == lead)) {
            kinds.rotate_left(pos);
        }
        kinds
    }

    /// The tick ran out of allowance while `order[at]` was running. When that
    /// kind drew nothing this tick it is the one left waiting; otherwise it made
    /// progress and the next kind in the run order is.
    pub(super) fn allowance_ended(&mut self, order: &[EmbeddingKind], at: usize, drawn_by_current: usize) {
        self.lead = if drawn_by_current == 0 {
            order.get(at).copied()
        } else {
            order.get(at + 1).copied()
        };
        if let Some(kind) = self.lead {
            log::info!("cortex::embed: kind={kind:?} waited on the tick allowance; it leads the next tick");
        }
    }

    /// The tick ended for any reason other than the allowance (backlog empty,
    /// a zero-progress batch, a stop): nothing is waiting.
    pub(super) fn clear(&mut self) {
        self.lead = None;
    }
}

#[cfg(test)]
mod tests;
