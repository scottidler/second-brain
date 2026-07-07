# Implementation Notes: Distillation Output Restore

Design doc: `docs/design/2026-07-07-distillation-output-restore.md`

## Phase 0: Prove the enumeration prompt on the April baseline

### Design decisions
- Draft patterns live at `docs/design/2026-07-07-distillation-output-restore-patterns/` (`distill-video.md`, `distill-video-chunk.md`, `distill-video-reduce.md`) - NOT in `borg/patterns/` - because landing them there is Phase 4's job and `otto deploy` syncs `borg/patterns/` to the live `~/.config/sb/patterns/`. Phase 4 picks them up from the drafts dir.
- Proof ran the REAL raw transcript, not only the recovered legacy body. `ytx KjEFy5wjFQg -f json` (Whisper) recovered the actual 16-minute transcript of the April video; converted to the production `[HH:MM:SS] text` line format. The recovered legacy body (demoted under `## Transcript` in the vault note) already contains the April Enumerated Points verbatim, so it alone is a weak test - the real transcript makes the model extract the enumeration from spoken content. Both inputs were run single-call; the real transcript fed the chunk -> reduce simulation. Reproduce the input with: `ytx KjEFy5wjFQg -f json` then prefix each segment with its `[HH:MM:SS]` start.
- Chunk -> reduce simulation: 4 line-boundary chunks (~48 lines each) via `split -n l/4`. Production would single-call this transcript (under `SINGLE_CALL_TOKEN_THRESHOLD`); the doc explicitly asks for a *simulated* chunk path. The split deliberately landed Firecrawl's discussion across chunks 01/02, producing 11 candidates for 10 items - the reduce deduplicated correctly, keeping the earliest anchor (00:07:33).
- Chunk pattern emits `declared_count` (chunk-level) + `enumeration_candidates` with an `ordinal` field (`#N` when the speaker numbers the item, `#?` when not). The reduce-input wire format (built by Phase 4's `build_reduce_input` extension) is a `## Enumeration Candidates` section: optional `Declared count: N` line, then one candidate per line as `[HH:MM:SS] #N name - text`. Section omitted entirely when no chunk found candidates.
- Reduce gate is evidence-based, default-null: even with candidates present, populate `enumeration` only when (a) a declared count exists, OR (b) most candidates carry real ordinals, OR (c) candidates span the timeline and are what the video is about. This rule earned its shape from a live failure: the first reduce run on the Herdr control FORCED a 2-item "UI sections" enumeration from two spurious chunk candidates (no declared count, no ordinals, one chunk). After strengthening the gate, the same input yields `enumeration: null` and the Top-10 positive case still yields 10/10.
- Fabric invocation mirrors production: `fabric -p <absolute pattern path> -m claude-sonnet-4-6` (`vault::fabric::resolve_pattern` passes absolute paths to `-p`), model = fabric's configured `DEFAULT_MODEL`.
- serde_yaml validation harness (scratch project, not committed) mirrored the design's data-model structs (`Enumeration { lead_in, declared_count, items }`, `EnumeratedItem { name, text, anchor }`, serde-defaulted `tldr`/`enumeration`/`key_ideas`). All five outputs parse and pass:
  - single-call real transcript: 10/10 items in creator order, declared_count=10, 10/10 anchored, tldr + lead_in + 5 key ideas
  - single-call recovered legacy body: 10/10 in order, 10/10 anchored
  - chunk -> reduce real transcript: 10/10 in order, declared_count=10, 10/10 anchored, Firecrawl dedup correct
  - single-call Herdr control (real dirty VTT text, per-word timing tags and rolling duplicates intact): `enumeration: null`
  - chunk -> reduce Herdr control: `enumeration: null` (gate holds against spurious candidates)

### Deviations
- Doc says "run it against the recovered April 'Top 10' transcript source"; the spike ran that AND the stronger real-transcript input (same effect, better evidence - the recovered body already contains the answer key).
- The negative control was also run through the full chunk -> reduce path, beyond the doc's single criterion. It caught the forced-enumeration failure the single-call path missed; Phase 4 should keep a chunked negative control in its tests.
- Phase 0 commit includes the design doc itself (it was untracked; a phase commit referencing a doc absent from history would dangle).

### Tradeoffs
- `ordinal` on chunk candidates vs bare candidate lists - chosen because the reduce needs creator order across chunk boundaries and anchors alone cannot distinguish "speaker numbered this" from "speaker mentioned this"; the Herdr failure showed ordinals are also gate evidence.
- One fabric run per case (no repeat-sampling) - the spike proves the prompt CAN do it; drift protection is Phase 7's eval metric, not repeated spike runs.
- Herdr control input used as-is with its VTT dirt (timing tags, rolling duplicates) rather than pre-cleaned - realistic worst-case input today; Phase 1 cleans it upstream.

### Open questions
- The recovered-legacy-body run reported 10/10 anchors, but that input has no `[HH:MM:SS]` transcript lines - the model lifted anchors from the video description's `M:SS`-format timestamp list and normalized them. Phase 4's anchor-honesty rule should decide whether description-derived anchors are acceptable or must be stripped.
- Whisper mishears "Claude Code" as "Cloud Code" throughout the real transcript; item names came through correctly regardless. If Phase 7 fixtures use ytx-recovered transcripts, expect that dirt in fixture text.

## Phase 1: Fix parse_vtt_segments

### Design decisions
- Replaced the literal `.replace("<c>", "").replace("</c>", ...)` chain with a shared `VTT_TAG_REGEX` (`</?c[^>]*>|<\d{2}:\d{2}:\d{2}\.\d{3}>`) in a new `strip_vtt_tags` helper (`borg/src/youtube.rs`), matching the doc's exact regex ask. `<i>`/`</i>` stay a literal replace inside the same helper (unchanged behavior, not itemized in the doc but present in the original code - kept so italic markup does not regress).
- Extracted the rolling-overlap decision (`extends -> replace, covered-by -> skip, dup -> skip`) that already lived inline in `clean_vtt` into a standalone `rolling_dedupe_action(last: Option<&str>, candidate: &str) -> RollingAction` function, and call it from both `clean_vtt` and `parse_vtt_segments`. This is the "port... reuse/share it rather than reinventing" instruction: one function now owns the collapse rule instead of two independently-maintained copies (which is exactly how `parse_vtt_segments` fell behind `clean_vtt` in the first place per the doc's compounding-bug paragraph).
- In `parse_vtt_segments`, a `Replace` action keeps the *earliest* start timestamp (the first cue where the growing line began) rather than the latest cue's start - the replacement text is the same utterance, just more complete, so its earliest-known start time is the semantically correct anchor for `slides::bind_transcript` callers.
- Added function-level debug logging to `parse_vtt_segments` (entry: byte length; exit: segment count) and `clean_vtt` (entry: byte length; exit: collapsed line count / result length), matching the file's existing instrumentation pattern; neither had any logging before.

### Deviations
- None. Implemented at the seam the doc named (`borg/src/youtube.rs`, `parse_vtt_segments` + `clean_vtt`), no new crate, no behavior change to unrelated functions.

### Tradeoffs
- Shared `rolling_dedupe_action` returns an enum (`Push`/`Replace`/`Skip`) rather than mutating the caller's `Vec` directly - chosen so the same decision function serves `clean_vtt`'s `Vec<String>` and `parse_vtt_segments`'s `Vec<(f64, String)>` without either one adapting its data shape to the other.
- Kept the pre-existing match-arm ordering where `candidate.starts_with(last)` is checked before the exact-equality arm (so an exact-duplicate candidate takes the `Replace` branch, not the dead `Skip` arm below it) - this mirrors `clean_vtt`'s original code exactly (the equality arm was already unreachable there) and is a no-op in practice since replacing identical text with itself does not change the accumulated value.

### Open questions
- None.


## Phase 2: Distilled contract + render

### Design decisions
- Added `tldr: Option<String>`, `enumeration: Option<Enumeration>`, `key_ideas: Vec<String>` to `vault::distilled::Distilled`, all `#[serde(default)]` (Options also `skip_serializing_if`) so every legacy staged `distilled.yml` deserializes unchanged (`vault/src/distilled.rs`). New structs `Enumeration { lead_in, declared_count, items }` and `EnumeratedItem { name, text, anchor }`, kebab-case serde, `anchor` carrying the same semantics as `Claim.anchor`. No `best_quotes` field (Resolved Decision 2026-07-07: `Claim.quote` already carries verbatim quotes).
- Placed the three new body fields between `summary` and `claims` in the struct so serialized YAML reads in note-body order (summary -> tldr -> enumeration -> key-ideas -> claims). Order is cosmetic for serde but keeps staged files legible.
- Rewrote the `Distilled.transcript` doc comment (`vault/src/distilled.rs`) to split the invariant: the FIELD stays populated (regression-guarded, do NOT revert to `None` - staging + embedding source), while the RENDER is now caller-gated. Pinned the new truth: a video/article note carries `transcript: Some(..)` with NO `## Transcript` section, and that is correct by design.
- `render` grew an explicit `options: RenderOptions { include_transcript: bool }` parameter (`distillers/src/render.rs`) - the seam the doc specified. Section order restored to the April shape: `> [!tldr]` callout -> `## Summary` (heading unchanged) -> `## Enumerated Points` (only when items present) -> `## Key Ideas` (omitted when empty) -> `## Claims` -> `## Links` -> `## Transcript` (only when `include_transcript`). `## Why Captured` is prepended by the borg markdown layer above this body, out of render's scope.
- tldr renders as a two-line Obsidian callout (`> [!tldr]\n> <hook>`); `cortex::quality` keys on the literal `> [!tldr]` marker (verified `quality.rs:304`). Enumerated items render `N. **Name**: text [anchor]` - hyphen/colon separators, NO em dash (safety rule). Empty/whitespace lead-ins, items, and key-ideas entries are filtered so no empty section or stray text leaks.
- `RenderOptions::for_url_publish(distilled)` (`distillers/src/render.rs`) encapsulates the one render site with logic: borg's single URL render site handles Video/Article/Repo/Thread, and only Thread (a verbatim kind, always attaches `KindPayload::Thread`) keeps its transcript; Video/Repo (payload present) and Article (payload `None`) render transcript-free. Keyed on the typed `KindPayload`, never on an extractor string. Extracted as a constructor so it is unit-testable per URL kind.

### Deviations
- The doc's six-site policy table lists `pipeline.rs:895` as "(URL: video/article/repo) -> false" and lists Thread separately under the always-`true` group. In the actual code all four URL kinds (Video/Article/Repo/Thread) render at the single `pipeline.rs` site; Thread does NOT have its own render seam. Same effect, correct seam: implemented the kind-aware policy via `RenderOptions::for_url_publish` keyed on `KindPayload`, so Thread gets `true` and Video/Article/Repo get `false` through one site. This is the only structural difference from the table; all six call sites are wired.
- Six production call sites confirmed and wired: `borg/src/pipeline.rs` URL site (`for_url_publish`, kind found: Video/Article/Repo/Thread), `borg/src/pipeline/text.rs` text/idea (`true`), `text.rs` vocabulary (`true`), `borg/src/pipeline/handlers.rs` image (`true`), `handlers.rs` audio/voicenote (`true`), `cortex/src/summarize.rs::rewrite_note_file` backfill (`true`, hardcoded with a load-bearing comment - backfill is the sole caller and is always-true).
- Inverted three borg pipeline tests that pinned the old "render always emits `## Transcript` for a Some transcript" behavior, renaming them to the new truth: `article_published_body_omits_transcript_but_yields_claims_fts_text`, `gate_article_transcript_on_keeps_field_but_publish_omits_section`, `article_transcript_gate_is_article_only_video_field_unaffected`, plus the slide-append test now asserts NO transcript (design line 44: slide notes keep sections "minus the transcript"). The `gate_article_transcript` function itself is unchanged in Phase 2 (Phase 3 re-scopes/removes it and its toggle).

### Tradeoffs
- Chose to default the three new fields in every production distiller builder (`tldr: None, enumeration: None, key_ideas: Vec::new()`) rather than `..Default::default()`, keeping those builders' explicit-enumerate style and signaling that Phase 4 (not Phase 2) is where the distillers populate them. Test fixtures use `..Default::default()` to reduce churn.
- The four verbatim borg sites (text/idea, vocabulary, image, audio) pass a literal `RenderOptions { include_transcript: true }`; their per-site policy tests (`site2..site5_*` in `render/tests.rs`) assert render behavior under that policy value rather than driving the deep async handlers end-to-end. The handlers have no existing unit fixtures, so a true integration test per site would be large; the render-behavior tests plus the `for_url_publish` matrix test (site 1) and the cortex `backfill_render_keeps_transcript_section` test (site 6) give bitey coverage of the actual policy each site uses.

### Open questions
- None.
