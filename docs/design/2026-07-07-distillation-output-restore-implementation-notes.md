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

