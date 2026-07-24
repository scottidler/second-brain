# Design Document: harvest distill-parsing robustness

**Author:** Scott Idler (via agent)
**Date:** 2026-07-24
**Status:** Implemented (code; Phase 4 replay is post-deploy)
**Review Passes Completed:** 5/5 + review-panel x2 (Architect + Staff Engineer) + consensus loop

## Summary

Borg's harvest run distills dormant clyde sessions into vault notes. The
pipeline mostly works (the large majority land `fallback=none` with 8-11
claims), but two tail failure modes -- both the SAME class, model YAML the strict
parser rejects -- degrade notes to an impoverished body. Single-call distills
fail as `yaml-parse-error` (5 traces); chunked distills fail as
`partial-chunk-failure` (~13 traces), confirmed to be overwhelmingly chunk-YAML
PARSE failures, not call failures. This makes the distill YAML parse tolerant of
the observed model-drift shapes, applies it as a class across all distillers AND
the chunk path, adds a bounded per-chunk retry, and replays the affected traces.

A third failure from the same `sb doctor` pass -- a session reaped as `crashed`
-- is a cross-process watchdog bug, orthogonal to parsing, tracked in its own doc
(`2026-07-24-harvest-watchdog-cross-process-reaping.md`).

## Problem Statement

### Background

- `sb borg harvest` distills sessions via fabric (`distill-session`,
  `claude-sonnet-5`) and parses the model's YAML into the `Distilled` contract.
  Small sessions take one call (`distillers/src/session.rs` single-call path);
  large ones are chunked and reduced (`distill_long`, `:258-408`).
- On a parse problem the code writes a fallback distillation
  (`distillers/src/validate.rs::fallback_distilled`), records the note
  `status=succeeded degraded=true` with a `fallback-reason`, and (for single-call)
  preserves the raw model output under `meta.validation.raw-output`. That capture
  is the durable evidence this rests on. NOTE: chunked (`distill_long`) traces do
  NOT preserve per-chunk raw-output (`raw-output: null`, e.g. hv-da5fec) -- their
  evidence is the `chunk yaml parse failed: ...` WARN lines in `borg.log`.

### Problem

Two source-confirmed failure modes, one root class (`serde_yaml` strictness vs
model output drift):

1. **`yaml-parse-error` (5 traces, single-call).** The model call succeeds but
   its output carries a duplicate mapping key that `serde_yaml` rejects, so the
   WHOLE document fails to parse -> fallback (claims=0). Two distinct duplicate
   shapes verified in staged `raw-output`:
   - `hv-c8d6b2`: `quote: "<real>"` then `quote: null` (value + null).
   - `hv-ee6ccc:32-35`: `kind: position` then `kind: position` (EQUAL non-null
     duplicate, on a key OTHER than `quote`).
   So the repair must handle duplicates of ANY key, in three value shapes (below).

2. **`partial-chunk-failure` (~13 traces, chunked).** Each chunk gets one fabric
   attempt; a chunk whose YAML fails to parse is dropped (`continue`,
   `session.rs:316`, `any_chunk_failed=true`) and the reduce keeps only survivors,
   flagging degraded. The dominant failure is a PARSE failure, not a call failure:
   `borg.log` shows ~13 `chunk yaml parse failed` vs 2 `chunk fabric call failed`.
   The parse failures are a MIX: some are the repairable duplicate/prose shapes;
   many are transient malformed output (`did not find expected key`, `mapping
   values are not allowed`, alias-scan) that a RETRY rescues, not the repair. No
   per-chunk retry exists today.

Both are the same class: model output the strict parser refuses. The single-call
path already survives ONE drift shape (it wraps input in `strip_fences`); it does
not survive duplicate keys or prose preambles, and the chunk path has no repair
OR retry.

### Goals

- A distill YAML carrying an observed drift shape (duplicate key of any name;
  prose preamble before the YAML) parses to a full `Distilled`, applied as a
  CLASS across all 15 parse sites AND the chunk path.
- A transiently-failing chunk (call error OR parse failure of any kind) is
  retried before it degrades the reduce.
- The repair is SAFE: it never silently keeps the wrong value, and it operates
  STRUCTURALLY (never a naive string replace that could corrupt a scalar). It
  fails LOUD on an ambiguous conflict.
- One regression test per failure mode, each proven to bite.
- Replay the affected degraded traces so the fix lands on real notes.

### Non-Goals

- **The `crashed` trace (`hv-741468`)** -- cross-process watchdog bug, separate
  doc (Scott, 2026-07-24: split).
- **Weakening the gate-2 quality gate** -- the 2 `fetch-failed` bot-block-page
  rejections are working-as-designed; parked.
- **Swapping `serde_yaml`** -- deprecated; the successor swap is separate
  dependency hygiene (Alternative 2). A lenient allow-dup parser would ALSO
  silently pick a value, which is the exact property we do NOT want on a conflict.
- Re-architecting the distiller or the chunking policy.

## Proposed Solution

### Overview

One shared tolerant-parse helper, routed across every distiller parse site and
the chunk path, plus a bounded per-chunk retry. The repair is UNCONDITIONAL (no
config kill switch): it is fail-loud-safe by construction, so it needs no toggle,
and gating it would require plumbing a flag from the borg config layer into the
config-free `distillers` crate for no safety benefit. Deterministic/cheap work
(tolerant parse, prompt belt) before the LLM-touching retry. Libs stay lib-only;
only sb prints; borg stays the sole staging writer.

### Architecture

- **Shared tolerant parse.** New `distillers/src/parse.rs`:
  `parse_pattern_yaml<T: DeserializeOwned>(raw: &str) -> Result<T, ParseError>`.
  Pipeline: (1) strict `serde_yaml::from_str`; (2) on failure, apply bounded
  STRUCTURAL repairs for the observed drift shapes and retry once; (3) if it
  still fails, return the parse error (existing fallback fires -- fail loud).
  - **Repair mechanism is structural, not string/regex** (panel finding: a raw
    replace can mutate a legitimate `quote: null` inside a multiline string
    block). The exact structural seam is a Phase 0 deliverable because
    `serde_yaml` may reject duplicate keys even when deserializing to the untyped
    `Value`: the candidate mechanisms, in preference order, are (a) parse the YAML
    event/node stream and dedupe mapping keys before building the typed value; (b)
    if `serde_yaml::Value` tolerates duplicates, dedupe on that tree; (c) failing
    both, an INDENT-AWARE line pass that only ever touches lines matching a
    mapping-key at a known indent and NEVER text inside a scalar block. Phase 0
    picks the one that works and proves it against the real artifacts.
  - **Prose-preamble strip (scoped).** Strip leading lines ONLY up to the first
    UNINDENTED root-level mapping key, matched generically as `^[A-Za-z0-9_-]+:`
    after optional BOM / `#` comments / `---`. It must NOT match a `summary:`-like
    line embedded inside prose, nor an indented claim key. It does NOT hardcode
    `Distilled` field names (no leaky abstraction). Composes AFTER `strip_fences`.
  - **Duplicate-key dedupe -- full invariant table, applied to EVERY key** (not
    just `quote`):
    | duplicate values | action |
    |---|---|
    | (value, null) / (null, value) | keep the non-null value; WARN |
    | equal non-null (e.g. `kind: position` x2) | keep one; WARN |
    | differing non-null | do NOT guess -> return parse error (fail loud) |
  Mirrors the in-house precedent `vault/src/distilled.rs:154-174` (custom
  `ClaimKind` Deserialize absorbing one bad enum value) -- "absorb KNOWN drift,
  WARN, else fail loud."
- **Route all 15 parse sites** through the helper (article 3, thread 3, session
  3, video 2, voicenote 2, image 1, repo 1 -- `PatternYaml`/`ReduceYaml`/
  `ChunkYaml`/`ThreadReduceYaml`, the `let parsed: T = serde_yaml::from_str(...)`
  type-ascription form). Preserve each site's existing `strip_fences` pre-step.
  The helper lives in `distillers` and takes no config (repair unconditional).
- **Chunk path (Bug 2).** The chunk parse in `distill_long` (`session.rs:~316`)
  routes through the SAME helper (rescues duplicate/prose shapes) AND is wrapped
  in a bounded retry: a chunk that errors OR fails to parse is retried up to
  `chunk-retries` times (the retry rescues the transient malformed-output shapes
  the repair does not cover). The retry re-issues ONLY after the prior attempt
  has fully returned (`session.rs:276-295` awaits its `spawn_blocking` at
  `fabric.rs:78`) -- verified non-overlapping; the earlier "async-timeout drops
  the child" finding does NOT apply here.

### Data Model

One new config knob, correctly wired across the crate boundary:

```yaml
distill:                        # borg/src/config.rs DistillConfig (deny_unknown_fields)
  chunk-retries: 1              # retries per failed/unparseable sub-chunk (default 1)
```

- `DistillConfig` (`borg/src/config.rs:282`, `deny_unknown_fields`) grows
  `chunk_retries: usize` (default 1). It must be added to the STRUCT in the same
  commit as the example, or `deny_unknown_fields` rejects the key at load.
- **Plumbing (panel must-fix #1):** `distillers` is config-free by design
  (`config.rs:265-266`). `chunk_retries` threads from `borg::DistillConfig` into
  the distillers `SessionConfig` (`distillers/src/session.rs:98-105`, add the
  field), populated by the borg pipeline constructor
  (`borg/src/pipeline/session.rs:~142`, which today passes only
  model/max_chars/timeout/token_cap). Only the SESSION path needs it (chunk retry
  is session-specific), so the plumbing is one constructor, not all 15 sites.
- **NO `yaml_repair` config.** The repair is unconditional (fail-loud-safe); it
  needs no cross-crate flag. (Resolves the "kill switch not wired" finding by
  removing the switch, not by plumbing an unused one.)
- kebab in YAML, snake in Rust via serde rename. `chunk-retries` default 1 is a
  behavior CHANGE from today's one-call-per-chunk (it adds a retry) -- intended.

### API Design

- `distillers::parse::parse_pattern_yaml<T>(raw) -> Result<T, ParseError>` --
  strict-then-structural-repair, no config arg. Returns the SAME error type the
  15 sites already handle when repair does not apply or a non-null conflict is
  detected, so a genuinely malformed input still fails loud to the fallback.
- `SessionConfig` (distillers) grows `chunk_retries: usize`; the chunk retry is
  internal to `distill_long`. No other public surface.

### Implementation Plan

#### Phase 0: Spike -- pick the structural repair mechanism, prove it on real data
**Model:** opus
- Zero production code. (1) Determine whether `serde_yaml::from_str::<Value>`
  tolerates duplicate keys; pick mechanism (a)/(b)/(c) accordingly. (2) Prove the
  chosen dedupe on `hv-c8d6b2` (value+null `quote`) AND `hv-ee6ccc:32-35` (equal
  non-null `kind`): both deserialize to a full `Distilled` with the real values
  preserved and >= 1 claim. (3) Prove a DIFFERING-non-null duplicate FAILS
  (returns Err, does not guess). (4) Source a prose-prefixed chunk from
  `borg.log` (the `"...Let me construct the YAML now."` case) or hand-build the
  fixture -- chunk traces have `raw-output: null`, so it CANNOT come from staging
  -- and prove the scoped preamble-strip makes it parse while leaving a
  prose-with-embedded-`summary:` string failing loud.
- **Success criteria:** mechanism chosen and justified (event-stream / Value-tree
  / indent-aware line pass); value+null and equal-non-null dedupe both preserve
  data; differing-non-null fails loud; scoped prose-strip parses the real
  preamble case and does NOT eat an embedded key. All at a scratch test, no
  production edit.

#### Phase 1: Shared tolerant parser + route the 15 sites
**Model:** opus
- Implement `parse.rs::parse_pattern_yaml` per the Phase 0 mechanism (structural
  dedupe with the full invariant table + scoped prose-strip; WARN on every
  repair). Route all 15 `serde_yaml::from_str` sites through it, preserving
  `strip_fences`.
- **Success criteria:** regression tests parse `hv-c8d6b2` (real quote kept) AND
  an equal-non-null `kind` duplicate (one kept) to full claims; a differing
  non-null duplicate returns the parse error; a prose-with-embedded-key string
  still fails loud; a plain malformed YAML still fails loud; `cargo test
  --workspace` green.

#### Phase 2: Prompt belt
**Model:** sonnet
- Add "emit each mapping key exactly once; output ONLY the YAML, no preamble" to
  `distill-session{,-chunk,-reduce}.md`; sync to `~/.config/sb/patterns/`. Belt,
  not load-bearing.
- **Success criteria:** the exact new line is present in all three repo patterns
  AND in `~/.config/sb/patterns/` after an explicit sync step (asserted).

#### Phase 3: Chunk-path tolerant parse + bounded retry
**Model:** opus
- Route the chunk parse in `distill_long` through `parse_pattern_yaml`; add
  `chunk_retries` (plumbed per Data Model) bounded retry around chunk call+parse.
- **Success criteria:** a chunk returning a duplicate/prose-shaped YAML yields a
  full result with `fallback=none` (repair path); a chunk returning transient
  NON-repair-shape malformed YAML then valid YAML succeeds via RETRY (not repair);
  exhausted retries degrade cleanly (no panic); an in-flight-counter fake asserts
  no retry OVERLAPS a still-running call (a barrier fake, since
  `set_response_sequence` proves order/count but not non-overlap).

#### Phase 4: Rollout -- replay affected traces (operator-run)
**Model:** sonnet
- Enumerate the exact affected trace IDs at replay time
  (`sb borg log --degraded --since <window> --stage ...` -> the 5 yaml-parse +
  ~13 partial-chunk). Replay each via `sb borg replay <trace> --from-stage 2`
  (session traces re-derive from staged `body.txt` + `attachments/members.yml`,
  `replay.rs:382-456`; verified present on `hv-c8d6b2`).
- **Success criteria:** each ENUMERATED trace lands `fallback=none`; assert on the
  explicit trace-ID list (not `degraded_24h`, unstable if unrelated degraded
  traces share the window).

## Acceptance Criteria

- [ ] `hv-c8d6b2` (value+null `quote`) parses with the real quote kept; a
      duplicate `kind: position` x2 parses keeping one; both yield >= 1 claim.
- [ ] A duplicate key with two DIFFERING non-null values returns the parse error
      (fails loud), never a silently-picked value.
- [ ] A prose-preamble-prefixed chunk parses to a full chunk result; a
      prose-with-embedded-`summary:` string still fails loud (strip is scoped).
- [ ] A transient non-repair-shape malformed chunk succeeds via bounded retry;
      an in-flight-counter fake proves no retry overlaps a running call.
- [ ] `DistillConfig` loads `chunk-retries`, threads it into distillers'
      `SessionConfig`, and an unknown key still fails loud (deserialization test).
- [ ] `otto ci` green; one regression test per failure mode, each proven to fail
      against pre-fix code.
- [ ] After Phase 4, the enumerated replayed trace IDs report `fallback=none`.

## Resolved Decisions

- **2026-07-24 -- fix Bug 1 + Bug 2 here; split Bug 3 (Scott).** Bug 3 is a
  cross-process watchdog issue, its own doc.
- **2026-07-24 -- Bug 1 + Bug 2 are ONE class (panel x2, verified).** ~13
  partial-chunk-failures are dominantly chunk-YAML PARSE failures. Fix = tolerant
  parser on the chunk path + retry around call AND parse.
- **2026-07-24 -- repair is non-null-wins + equal-collapse + fail-loud, NOT
  last-wins (panel).** Full invariant table applied to EVERY duplicate key (not
  just `quote`); a real case is `hv-ee6ccc`'s equal-non-null `kind` duplicate.
  Last-wins would drop the real `quote` value (silently lossy).
- **2026-07-24 -- repair is STRUCTURAL, not string/regex (panel).** A naive
  replace can corrupt a `quote: null` inside a multiline scalar; the exact
  structural mechanism is a Phase 0 deliverable (event-stream / Value-tree /
  indent-aware line pass).
- **2026-07-24 -- repair is UNCONDITIONAL, no config kill switch (consensus).**
  Fail-loud-safe by construction; a cross-crate flag into config-free `distillers`
  buys nothing. Resolves the "kill switch not wired" finding by removing it.
- **2026-07-24 -- chunk-retries plumbed borg->distillers via SessionConfig
  (panel must-fix).** `DistillConfig.chunk_retries` -> `SessionConfig.chunk_retries`
  through the `pipeline/session.rs` constructor; session-only, one path.
- **2026-07-24 -- prose-strip scoped to first unindented root key (panel).**
  Generic `^[A-Za-z0-9_-]+:`, never an embedded/indented key; negative test
  required; no hardcoded Distilled field names.
- **2026-07-24 -- prompt is a belt (research).** Parser is load-bearing.
- **2026-07-24 -- serde_yaml successor swap parked (Alternative 2).**
- **2026-07-24 -- replay traces enumerated, not counted (panel).** Assert on the
  explicit ID list; the ~18 (5 + ~13) are `--from-stage 2` replayable.

## Alternatives Considered

### Alternative 1: fix only the distill-session prompt
- **Cons:** pattern is already correct; model drifts anyway; leaves the chunk path
  and 14 other sites exposed. **Why not:** prompt is a belt.

### Alternative 2: swap serde_yaml for a lenient YAML crate
- **Cons:** dependency swap across 15 sites + the whole `Distilled` parse; and a
  lenient allow-dup would silently PICK a value -- the exact fail-loud property we
  want on a conflict. **Why not:** parked as dependency hygiene; local structural
  repair is minimal and safer.

### Alternative 3: last-wins dedupe
- **Cons:** observed drift is `quote: "<real>"` then `quote: null` -- last keeps
  null, drops the real quote. **Why not:** violates fail-loud; invariant table is
  the correct rule.

## Technical Considerations

### Dependencies
- `distillers` (parse, chunk retry, SessionConfig), `borg::config` (DistillConfig)
  + `borg::pipeline::session` (constructor plumbing), `borg::replay` (rollout). No
  new external crates.

### Performance
- Repair runs only on the rare parse-error path; negligible. Retries bounded
  (default 1), only on failure; worst case one extra fabric call per failing chunk.

### Security
- No new secrets, no network. Local vault + staging only.

### Testing Strategy
- `distillers` regression tests: verbatim `hv-c8d6b2` (value+null), equal-non-null
  `kind`, differing-non-null (fail loud), prose-with-embedded-key (fail loud),
  scoped-prose-strip (parses), `FakeFabric` transient-then-valid chunk (retry),
  in-flight-counter barrier (non-overlap). Config deserialization test (new key
  loads via the plumbing; unknown key fails). Each test bites against pre-fix code.

### Rollout Plan
- Ship in the sb binary via direct-to-main + tag. `otto deploy` syncs pattern
  lines + `borg.yml` example, restarts daemons. Phase 4 replay is an explicit
  operator step on the daemon host.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Repair silently keeps the wrong value | Low | High | Invariant table: non-null-wins, equal-collapse, FAIL LOUD on differing non-null; WARN on every repair; differing-non-null regression test |
| String-level repair corrupts a scalar | Med | High | STRUCTURAL repair (Phase 0 mechanism), never a raw replace; scalar-content untouched |
| Prose-strip eats a legitimate key | Low | Med | Scoped to first UNINDENTED root key; negative test (embedded `summary:` still fails loud) |
| chunk_retries not wired / breaks load | Med | Med | Struct field + example in one commit; plumbed borg->distillers; deserialization test |
| Chunk retry amplifies load in an outage | Low | Med | Bounded (default 1), only on failure, non-overlapping, tunable to 0 |

## Open Questions

None. Both failure modes are root-caused against source + logs. The one
implementation unknown -- the exact STRUCTURAL dedupe mechanism (event-stream vs
Value-tree vs indent-aware line pass, gated on whether serde_yaml tolerates
duplicate keys in `Value`) -- is a Phase 0 spike deliverable proven against the
real artifacts before any production edit; the design is robust to whichever the
spike selects. Bug 3 is split to its own doc.

## References

- `distillers/src/session.rs:98-105,258-408` (SessionConfig, `distill_long`, chunk drop `:316`, retry site `:276-295`)
- `distillers/src/fabric.rs:78,142` (awaited `spawn_blocking`; `FakeFabric`)
- `distillers/src/validate.rs` (`fallback_distilled`, raw-output capture)
- `vault/src/distilled.rs:154-174` (in-house drift-absorbing Deserialize precedent)
- `borg/src/config.rs:265-266,282-283` (gates-at-borg-layer note; `DistillConfig`, deny_unknown_fields)
- `borg/src/pipeline/session.rs:~142` (SessionConfig constructor -- plumbing seam); `borg/src/lib.rs:49` (`DistillInputs`)
- `borg/src/replay.rs:382-456` (`replay_session_stage2`)
- `borg/patterns/distill-session{,-chunk,-reduce}.md`
- Evidence: `~/.local/share/sb/borg.log` (chunk parse-fail WARNs), `~/.local/share/sb/borg/stages/{hv-c8d6b2,hv-ee6ccc}/distilled.yml`
- Companion: `docs/design/2026-07-24-harvest-watchdog-cross-process-reaping.md`
