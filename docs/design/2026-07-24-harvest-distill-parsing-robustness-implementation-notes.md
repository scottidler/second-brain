# Implementation Notes: harvest distill-parsing robustness

Companion to `2026-07-24-harvest-distill-parsing-robustness.md`. Append-only;
one section per phase.

## Phase 0: Spike -- pick the structural repair mechanism, prove it on real data

Empirical result and mechanism decision (no production code; the spike test was
a throwaway `distillers/tests/spike_scratch.rs`, deleted before commit).

### Spike findings

- **`serde_yaml` 0.9.34 REJECTS duplicate mapping keys even when deserializing
  to the untyped `serde_yaml::Value`.** Verified directly:
  - `serde_yaml::from_str::<Value>("a: 1\na: 2\n")` -> `Err("duplicate entry with key \"a\"")`.
  - A nested sequence-item dup (`claims: [ { kind: position, kind: position } ]`)
    -> `Err(claims[0]: duplicate entry with key "kind" ...)`.
  - Both real artifacts fail strict parse to `PatternYaml`:
    `hv-c8d6b2` -> `claims[0]: duplicate entry with key "quote" at line 3 column 5`;
    `hv-ee6ccc` -> `claims[0]: duplicate entry with key "kind" at line 3 column 5`.
- **Therefore mechanism (b) (dedupe on a tolerant `Value` tree) is impossible**
  -- there is no tolerant untyped tree. **Mechanism (a) (parse the libyaml
  event/node stream and dedupe)** is not reachable either: `serde_yaml` 0.9 does
  not expose its `unsafe-libyaml` event stream publicly, and the design's
  Dependencies section forbids new external crates. So the design's fallback,
  **mechanism (c) -- an INDENT-AWARE line pass -- is the chosen mechanism**, and
  the design explicitly anticipated this outcome.
- **Prose-preamble case sourced from `borg.log` (real):**
  `chunk yaml parse failed: invalid type: string "Given the truncation marker, I
  need to distill what's present without speculating about the missing middle
  section. Let me construct the YAML now.\nThe chunk covers"`. This confirms the
  model emits leading prose that makes the whole document parse as a scalar
  string. Chunk traces have `raw-output: null`, so (per the design) the Phase 1
  positive prose fixture is hand-built to this shape.

### Proof (realized as the Phase 1 regression tests, which read the verbatim artifacts)

The verbatim `raw-output` of `hv-c8d6b2` and `hv-ee6ccc` was extracted (dedented
out of `meta.validation.raw-output`) into `distillers/tests/fixtures/` and is
parsed by the Phase 1 tests:
- value+null `quote` (`hv-c8d6b2`): repairs, keeps the real quote, >= 1 claim.
- equal-non-null `kind` (`hv-ee6ccc`): repairs, keeps one, >= 1 claim.
- differing-non-null duplicate: returns the parse error (fails loud, no guess).
- scoped prose-strip: the hand-built preamble parses; a prose blob whose only
  key-shaped line is INDENTED still fails loud.
- A `strict_parse_rejects_the_real_duplicate_artifacts` test proves the fixtures
  fail the pre-fix strict path, so the repair is load-bearing (the tests bite).

### Design decisions

- Mechanism (c), indent-aware line pass -- forced by the empirical `serde_yaml`
  result above; the safe, dependency-free option.

### Deviations

- The spike's "proof against real artifacts" is delivered as the Phase 1
  regression tests (same verbatim fixtures) rather than as a separate throwaway
  harness, since Phase 0 and Phase 1 were implemented in one context. Same
  evidence, no lost rigor -- the throwaway spike test that established the
  `serde_yaml` behavior was deleted as instructed.

### Tradeoffs

- None.

### Open questions

- None.

## Phase 1: Shared tolerant parser + route the 15 sites

### Design decisions

- `distillers::parse::parse_pattern_yaml<T: DeserializeOwned>(raw) -> Result<T,
  serde_yaml::Error>` -- strict parse; on failure apply structural repairs and
  retry once; else return the ORIGINAL strict error. `distillers/src/parse.rs`.
- **Error type is `serde_yaml::Error` (the type the 15 sites already handle),
  and the fail-loud paths return the ORIGINAL strict error** rather than
  synthesizing one. This means a differing-non-null conflict fails loud with the
  real `duplicate entry with key ...` message, and the `?`-sites still convert to
  `eyre::Report` unchanged. `parse_pattern_yaml` -- why: preserves both call
  forms (the `match { Err(err) }` sites and the `?` sites) with zero signature
  churn, and keeps the fallback-reason message truthful.
- **Repair order: scoped prose-strip, then indent-aware dedupe** -- prose blocks
  the parse entirely and would confuse the line scanner, so it is removed first;
  `repair_pattern_yaml`.
- **Duplicate-key invariant table via `resolve_duplicate`** -- (value,null)/
  (null,value) keep non-null; equal non-null keep one; equal null collapse;
  differing non-null OR any non-leaf (`Opaque`) value -> `Conflict` (fail loud).
  `parse.rs::resolve_duplicate` + `classify_value`.
- **Scalar-block bodies marked opaque up front (`mark_block_scalar_bodies`)** so
  the dedupe never inspects or deletes a `quote: null`-shaped line that is
  actually prose inside a `|`/`>` block (the panel's string-repair hazard).
  Threshold is the KEY's column, so sibling keys are never swallowed.
- **Sequence-element reset**: a `- ` marker at a column resets that column's
  seen-set, so item 2's keys never read as duplicates of item 1's
  (`dedupe_mapping_keys`, `has_dash` branch).
- **Prose-strip scoped to the first UNINDENTED root key** matched generically
  (`is_root_mapping_key`, `^word-chars:` at column 0, no hardcoded Distilled
  field names); a no-op when the doc already starts with a root key, so it never
  interferes with a duplicate-only failure.
- **Routed all 15 `serde_yaml::from_str` type-ascription sites** through the
  helper via the fully-qualified `crate::parse::parse_pattern_yaml`, preserving
  each site's `strip_fences` pre-step: article (3), thread (3), session (3),
  video (2), voicenote (2), image (1), repo (1).
- **WARN on every repair actually applied** (prose lines stripped; each deduped
  key with kept/dropped values + line number) per the logging rule.

### Deviations

- **WARN "key path" is the key NAME + 1-based line number + kept/dropped values,
  not a full `claims[0].quote` dotted path.** Same-effect diagnostics at the
  correct seam: a full path would require threading a path stack through the
  line walker for no operational benefit (the line number already locates it).
- **The session chunk-parse site (`session.rs:316`) is ROUTED through the helper
  in Phase 1 (it is one of the 15 sites) but the surrounding drop/`continue`
  retry structure is untouched** -- the bounded per-chunk retry is Phase 3, per
  the plan and the task brief.

### Tradeoffs

- **Hand-rolled indent-aware line pass** vs a new lenient-YAML dependency: chose
  the line pass because `serde_yaml`'s `Value` rejects dups (Phase 0) and the
  design forbids new crates. Mitigated by marking scalar-block bodies opaque and
  restricting repair to inline leaf scalars -- anything ambiguous fails loud.
- **Repair restricted to inline leaf scalars**: a duplicate whose value is a
  block scalar, a flow collection, or an empty (parent) key is treated as
  `Conflict` (fail loud) rather than attempting a structural merge. Conservative;
  matches the observed drift shapes and the fail-loud-safe invariant.

### Open questions

- None.

## Phase 2: Prompt belt (single-key, no-preamble)

Belt to the Phase 1 parser's suspenders: reduce the frequency of the two drift
shapes the tolerant parser recovers. Prose-only change; the parser stays
load-bearing.

### Design decisions

- Strengthened the existing "Output ONLY valid YAML ... no closing prose"
  RULES line in all three session patterns (`distill-session.md`,
  `distill-session-chunk.md`, `distill-session-reduce.md`) to (a) name the exact
  preamble failure ("no explanatory sentence before or after the YAML, e.g.
  'Let me construct the YAML now.'") and (b) add an explicit "emit each mapping
  key EXACTLY ONCE ... never two `quote:` or two `kind:` lines" rule, each
  naming that a violation fails the parse.
- Patterns are NOT embedded via `include_str!`; borg resolves them at runtime
  from `~/.config/sb/patterns/` (source of truth `borg/patterns/`). Synced the
  three edited files to `~/.config/sb/patterns/` explicitly (verified in sync via
  `diff`); that deployed copy is outside the repo and is not committed (it is a
  runtime target `otto deploy` also refreshes).

### Deviations

- None.

### Tradeoffs

- Extended the existing RULES lines rather than adding a new standalone rule
  block, to keep the pattern terse and avoid a vague duplicate instruction.

### Open questions

- None. (Completed by the orchestrator after the phase agent edited + synced the
  patterns but idled before committing; the belt text, the three-file sync, and
  this note were verified before commit.)

## Phase 3: Chunk-path tolerant parse + bounded retry

The chunk parse already routes through `parse_pattern_yaml` (Phase 1). Phase 3
adds the bounded per-chunk RETRY and the `chunk-retries` config plumbed across
the borg -> distillers boundary.

### Design decisions

- **`chunk_retries` config knob, default 1, plumbed borg -> distillers.**
  `DistillConfig.chunk_retries` (`borg/src/config.rs`, `deny_unknown_fields`,
  kebab `chunk-retries` via `rename_all`) is populated into
  `SessionConfig.chunk_retries` (`distillers/src/session.rs`) by the borg
  pipeline constructor (`borg/src/pipeline/session.rs`). `distillers` stays
  config-free — it receives the value, never reads config. Struct field + the
  `borg.yml.example` line landed in this one commit so `deny_unknown_fields`
  never rejects the key. Default 1 is a hand-written `impl Default` (not the
  `usize::default()` 0) so an absent key keeps the retry on.
- **Retry lives in a free fn `distill_chunk_with_retry`** (`session.rs`) that
  owns ONE chunk's `chunk_retries + 1` attempt loop: each attempt awaits
  `fabric.call` to completion, then parses; on call-error OR parse-failure it
  loops. Because the loop is sequential inside a single per-chunk task, no two
  calls for the same chunk are ever in flight — the design's non-overlap
  invariant holds by construction, not by luck.
- **`ChunkOutcome` enum replaces the raw `Result<String>` fan-out result.** The
  per-chunk task now returns `Parsed { output_chars, parsed }` or
  `Failed { output_chars }`; the sequential reduce loop maps `Parsed` to the
  claim/summary/link/tag merge and `Failed` to `any_chunk_failed = true` —
  byte-for-byte the old `partial-chunk-failure` semantics, just after the retry
  is exhausted rather than on first failure.
- **Logging (per `logging.md` + `rust.md`):** per-ATTEMPT failures are `trace!`
  (tight loop); the chunk-level DEBUG entry (`attempts=`) fires once; an
  EXHAUSTED chunk logs a single `warn!` naming the last error — preserving the
  `borg.log` chunk-failure evidence the design's Bug-2 diagnosis rests on.
- **`FakeFabric` grew `set_response_for_input` / `set_error_for_input`** (input
  substring, first-match, checked before the pattern-keyed steady response) so a
  two-chunk partial-failure test is DETERMINISTIC regardless of
  `buffer_unordered` completion order — the outcome rides the chunk's own body
  marker, not call ordering (the panel's `set_response_sequence` "order not
  overlap" caveat).
- **Non-overlap proven with a real concurrency probe.** A test-local
  `InFlightProbe` FabricCaller opens a sleep-widened in-flight window, records
  peak concurrent chunk calls, fails attempt 1 to force a retry, succeeds
  attempt 2; the test asserts `chunk_calls == 2` (a retry happened) AND
  `max_inflight == 1` (it never overlapped). This bites: a spawn-both-attempts
  implementation would read `max_inflight == 2`.

### Deviations

- **Retry tests drive `distill_long` DIRECTLY with hand-built chunk vectors**
  rather than the `distill()` entry. The fixed chunk-size constants
  (`SINGLE_CALL_TOKEN_THRESHOLD` 12K, `CHUNK_TOKEN_TARGET` 8K) make it impossible
  to produce a single controllable chunk through `distill()`, so the direct call
  is the correct seam to isolate one chunk's retry deterministically. Same effect
  (the real map-reduce path runs `distill_long`), correct seam.
- **`ChunkOutcome::Parsed.parsed` is `Box<PatternYaml>`** to satisfy clippy
  `large_enum_variant` (`PatternYaml` is ~264 bytes; `Failed` is 8). Cosmetic;
  the box is unwrapped (`*parsed`) at the single consumption site.

### Tradeoffs

- **Accumulate `output_chars` from the returned attempt** (successful attempt's
  raw len, or the last failed attempt's) rather than summing every attempt's
  bytes. `output_chars` feeds only the token-accounting meta field, not any
  load-bearing decision, so counting the final attempt keeps the metric honest
  without inflating it by the retried bytes.
- **Extended `FakeFabric` over adding a second fake.** Input-keyed matching is a
  small, general addition that other map-reduce tests can reuse, versus a
  bespoke throwaway fake; the dedicated `InFlightProbe` is kept only for the
  concurrency assertion it uniquely needs (a real awaited in-flight window,
  which `FakeFabric`'s lock-per-call model cannot express).

### Open questions

- None.
