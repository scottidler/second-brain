//! The per-tick chunk allowance and the note-boundary cap that fits a batch
//! to it.

/// How many chunks a batch may still embed this tick.
///
/// The daemon tick builds one from `embed.max-chunks-per-tick` and every batch
/// of every kind draws from it, in run order, so the cap bounds the whole tick
/// and not one batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkAllowance {
    /// `max-chunks-per-tick: 0`: no ceiling.
    Unlimited,
    /// Up to `remaining` chunks. `fresh` means nothing has been drawn yet this
    /// tick: only then may a single note larger than `remaining` embed alone,
    /// so a tick never exceeds max(cap, largest single note).
    Limited { remaining: usize, fresh: bool },
}

impl ChunkAllowance {
    pub fn per_tick(cap: usize) -> Self {
        if cap == 0 {
            ChunkAllowance::Unlimited
        } else {
            ChunkAllowance::Limited {
                remaining: cap,
                fresh: true,
            }
        }
    }

    pub fn is_spent(&self) -> bool {
        matches!(self, ChunkAllowance::Limited { remaining: 0, .. })
    }

    pub fn draw(&mut self, chunks: usize) {
        if let ChunkAllowance::Limited { remaining, fresh } = self
            && chunks > 0
        {
            *remaining = remaining.saturating_sub(chunks);
            *fresh = false;
        }
    }

    /// Notes a one-chunk-per-note kind (summary) may select this batch.
    pub(super) fn note_limit(&self, batch_size: usize) -> usize {
        match self {
            ChunkAllowance::Unlimited => batch_size,
            ChunkAllowance::Limited { remaining, .. } => batch_size.min(*remaining),
        }
    }
}

pub(super) struct TranscriptWork {
    pub(super) note_path: String,
    pub(super) chunks: Vec<String>,
    pub(super) source_modified_at: i64,
}

/// Drop whole notes off the tail of `work` until its chunk count fits the
/// allowance. [`ChunkAllowance::Unlimited`] keeps everything.
///
/// A single note larger than the allowance embeds alone only while the
/// allowance is fresh (nothing drawn yet this tick): it must still make
/// progress, or it would be skipped forever and its embeddings would never
/// exist. That one note is the deliberate exception to the bound, and it is
/// logged as such. On a partly drawn allowance the same note defers to the
/// next tick, which keeps a tick at or under max(cap, largest single note).
pub(super) fn cap_work_by_chunks(work: Vec<TranscriptWork>, allowance: ChunkAllowance) -> Vec<TranscriptWork> {
    let (cap, fresh) = match allowance {
        ChunkAllowance::Unlimited => return work,
        ChunkAllowance::Limited { remaining, fresh } => (remaining, fresh),
    };
    let total: usize = work.iter().map(|w| w.chunks.len()).sum();
    if total <= cap {
        return work;
    }
    let mut kept: Vec<TranscriptWork> = Vec::new();
    let mut running = 0usize;
    for w in work {
        let n = w.chunks.len();
        let oversized_exception = kept.is_empty() && fresh;
        if running + n > cap && !oversized_exception {
            break;
        }
        running += n;
        kept.push(w);
        if running >= cap {
            break;
        }
    }
    if kept.len() == 1 && running > cap {
        log::warn!(
            "cortex::embed: {} alone yields {} chunk(s), over the {} per-tick cap; embedding it anyway so it can converge",
            kept[0].note_path,
            running,
            cap,
        );
    }
    log::info!(
        "cortex::embed: per-tick allowance {} chunk(s): embedding {} note(s)/{} chunk(s) now, deferring the rest of {} total",
        cap,
        kept.len(),
        running,
        total,
    );
    kept
}

#[cfg(test)]
mod tests;
