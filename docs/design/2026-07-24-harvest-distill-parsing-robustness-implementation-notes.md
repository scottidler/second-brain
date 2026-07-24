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
