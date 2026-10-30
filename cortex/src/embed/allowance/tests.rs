use super::*;

// --- per-tick chunk ceiling (governance-starvation fix) ----------
//
// The incident: `batch_size` bounds NOTES, so a tick's real work was
// `64 notes x unbounded chunks`. One tick ran two days on `talos` (AVX-only),
// held the shared rayon pool, and starved classify. These pin the bound.

fn tw(path: &str, chunks: usize) -> TranscriptWork {
    TranscriptWork {
        note_path: path.to_string(),
        chunks: (0..chunks).map(|i| format!("chunk {i}")).collect(),
        source_modified_at: 100,
    }
}

fn total_chunks(work: &[TranscriptWork]) -> usize {
    work.iter().map(|w| w.chunks.len()).sum()
}

#[test]
fn cap_zero_disables_the_ceiling() {
    let work = vec![tw("a.md", 500), tw("b.md", 500)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(0));
    assert_eq!(out.len(), 2, "0 means no cap");
    assert_eq!(total_chunks(&out), 1000);
}

#[test]
fn work_under_the_cap_passes_through_untouched() {
    let work = vec![tw("a.md", 10), tw("b.md", 20)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 2);
    assert_eq!(total_chunks(&out), 30);
}

#[test]
fn truncates_on_a_note_boundary_never_mid_note() {
    // The write phase replaces a note's ENTIRE chunk set, so a partially
    // embedded note would land a truncated set and then read as complete.
    // Every kept note must therefore keep ALL of its chunks.
    let work = vec![tw("a.md", 300), tw("b.md", 300), tw("c.md", 300)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "b would overshoot 512, so it is deferred whole");
    assert_eq!(out[0].note_path, "a.md");
    assert_eq!(out[0].chunks.len(), 300, "the kept note keeps every chunk");
}

#[test]
fn packs_as_many_whole_notes_as_fit() {
    let work = vec![tw("a.md", 200), tw("b.md", 200), tw("c.md", 200)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 2, "400 fits, 600 does not");
    assert_eq!(total_chunks(&out), 400);
}

#[test]
fn a_single_oversized_note_still_makes_progress() {
    // Regression guard against a livelock: if the cap could defer a note that
    // alone exceeds it, that note would be deferred forever and never embed.
    let work = vec![tw("huge.md", 5000)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "the lone oversized note is the deliberate exception");
    assert_eq!(out[0].chunks.len(), 5000, "and it embeds whole, not truncated");
}

#[test]
fn oversized_first_note_does_not_drag_others_along() {
    let work = vec![tw("huge.md", 5000), tw("small.md", 5)];
    let out = cap_work_by_chunks(work, ChunkAllowance::per_tick(512));
    assert_eq!(out.len(), 1, "only the oversized note; the rest defer");
    assert_eq!(out[0].note_path, "huge.md");
}

#[test]
fn partly_drawn_allowance_defers_an_oversized_note() {
    let work = vec![tw("huge.md", 600), tw("small.md", 5)];
    let allowance = ChunkAllowance::Limited {
        remaining: 500,
        fresh: false,
    };
    assert!(cap_work_by_chunks(work, allowance).is_empty());
}

#[test]
fn allowance_draws_down_and_reports_spent() {
    let mut a = ChunkAllowance::per_tick(10);
    assert!(!a.is_spent());
    a.draw(0);
    assert_eq!(
        a,
        ChunkAllowance::Limited {
            remaining: 10,
            fresh: true
        },
        "drawing nothing keeps it fresh"
    );
    a.draw(4);
    assert_eq!(
        a,
        ChunkAllowance::Limited {
            remaining: 6,
            fresh: false
        }
    );
    assert_eq!(a.note_limit(64), 6);
    a.draw(99);
    assert!(a.is_spent(), "an overdraw saturates at 0");

    let mut unlimited = ChunkAllowance::per_tick(0);
    unlimited.draw(1_000_000);
    assert_eq!(unlimited, ChunkAllowance::Unlimited);
    assert!(!unlimited.is_spent());
    assert_eq!(unlimited.note_limit(64), 64);
}
