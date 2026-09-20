# Implementation Notes: Tags-Only Classification

Running, append-only record of how the implementation of
`docs/design/2026-09-19-tags-only-classification.md` diverges from or
interprets the design doc. One section per phase, four buckets each, "None."
where a bucket is empty. Never rewritten: a later decision that overrides an
earlier one is appended as a new entry that says so.

## Phase 1: Vocabulary, mapping, aliases, segment guard

### Design decisions

- **The five domain names went into their existing same-named groups, not a new `domains:` group** (`config/canonical-tags.yml`). The doc's P1 bullet says "`domains:` group `tech work life homelab diy`", but the doc also states the grouping is editorial and that nothing reads the keys (`CanonicalTagsFile::all_tags()` flattens `values()`, `vault/src/canonical.rs:33-35`). Putting `- tech` under the existing `tech:` group, `- work` under `work:`, and so on reads better and keeps every group's key equal to its own head tag. Flattened result is identical: 117 unique tags.
- **`CanonicalState` survives in borg as a thin wrapper** (`borg/src/pipeline.rs:46`). The doc says `CanonicalSet` "absorbs" it. `CanonicalSet` absorbed the three fields that are shared vocabulary (`all`, `no_segment`, `max_per_note`); `mapping` and `reject_concatenated` are borg-local and have no place in a shape cortex and distillers also load, so `CanonicalState` now holds `{ canon: CanonicalSet, mapping, reject_concatenated }`.
- **`no_classifier_tags` is parsed in P1, consumed in P2** (`vault/src/canonical.rs`, `CanonicalTagsFile`). The doc puts the YAML key in P1 and its only reader (`--retag`) in P2. Added the `#[serde(default)] pub no_classifier_tags: Vec<String>` field now so the vocabulary file round-trips through the struct in the same commit that introduces the key, rather than being silently ignored for one phase. It is deliberately NOT on `CanonicalSet`, whose three fields are exactly what the two matchers need.
- **`default_max_per_note()` still returns 7** (`vault/src/canonical.rs`). That serde default only applies to a file with no `max-per-note` key; the shipped file says 8. Changing the default would silently move the cap for any other caller that writes a partial file.
- **`alias_free_ai_survives_apply_tags` reads both on-disk list forms** (`cortex/src/tags/tests.rs`). The first version asserted a block-form `- ai` bullet and failed: `apply_tags` goes through `replace_tags_in_frontmatter`, which still writes the inline `tags: [ai, kubernetes]` form and only switches to block in P4. The code was right and the assertion was wrong, so the test now parses the `tags` value in either form via a local `frontmatter_tags` helper. It keeps passing across the P4 writer switch.
- **Two invariants added to `vault/AGENTS.md`**: the segment guard must be honored in *both* tier-2 loops (`match_to_canonical` and `filter_and_cap` each carry their own copy), and removing a tag from the vocabulary means deleting its `tag-mapping.yml` self-map in the same commit (panel r4 OQ9: the mapping is consulted before canonical membership, `canonical.rs:86-93`).

### Deviations

- **The `domains:` group named in the P1 bullet was not created.** See the first design decision above: the grouping is editorial, nothing reads the keys, and the flattened vocabulary is what the doc's own success criterion measures.
- **P1's success criteria were asserted against the in-repo `config/` source files, not the deployed `~/.config/sb/` copies.** The doc's greps target the deployed paths, which only exist after `otto deploy`; the deploy is an operator step this phase does not run. The in-repo files are the source of truth those copies come from. The deployed-path criteria are listed under operator steps below and stay unverified until the deploy runs.
- **`alias_free_ai_survives_apply_tags` tests the retirement, it does not perform it.** The three `ai`/`ML`/`ml` -> `ai-llm` aliases live in `~/.config/sb/cortex.yml`, which is dotfiles-managed and outside this repo. The test (`cortex/src/tags/tests.rs`) removes them from the loaded `TagsConfig` and asserts `apply_tags` then leaves a note's `ai` tag alone. The live edit is an operator step.

### Tradeoffs

- **`CanonicalSet` by value vs. threading three parameters.** The doc's API change was taken as written: `match_to_canonical(raw, &CanonicalSet, &TagMapping)` and `filter_and_cap(&[String], &CanonicalSet, &TagMapping)`. It churns every call site once (borg `pipeline/tags.rs`, cortex `sweep.rs`) in exchange for one loaded vocabulary shape that P2's distillers seam can take unchanged. The alternative, adding a fourth `no_segment: &HashSet<String>` parameter beside the existing three, would have avoided the churn but left three crates each assembling the vocabulary their own way.
- **`is_concatenated_word` still takes a bare `&HashSet<String>`.** It has no use for the guard list or the cap, so it kept its narrower signature; callers pass `&state.canon.all`. The test helper split into `test_canonical_hashset()` (for that function) and `test_canonical_set()` (for the two matchers) as a result.

### Open questions

- None. The operator steps below gate the *verification* of three P1 criteria, not the code.

### Deferred operator steps (not run by this phase)

1. dotfiles `~/.config/sb/cortex.yml`: delete the three `ai-llm` aliases (`:64-66`), so `grep -cE '^\s+(ai|ML|ml): ai-llm' ~/.config/sb/cortex.yml` == 0 (today 3).
2. `otto deploy`, to push `config/canonical-tags.yml` and `config/tag-mapping.yml` into `~/.config/sb/`, after which the doc's deployed-path greps can run.
3. `sb cortex sweep --migrate --dry-run`, expected to report 0 notes losing a domain-name tag.

### Environment note (not a phase deviation)

`otto ci` cannot use `sccache` from inside Claude Code's Bash sandbox on this
host, and the failure is the sandbox's, not sccache's. Traced:

```
connect(AF_INET, 127.0.0.1:4227)      = -1 ECONNREFUSED
socket(AF_UNIX, SOCK_STREAM|...)      = -1 EPERM
```

The sandbox gets its own network namespace, so the host's sccache server
(listening on `127.0.0.1:4227`) is unreachable; sccache then tries to start
its own server and the sandbox refuses `AF_UNIX` socket creation outright, so
there is no in-sandbox workaround (`SCCACHE_SERVER_UDS` fails at the same
`socket()` call). The same command run with the sandbox off exits 0. Listing
`cargo *` / `otto *` in `sandbox.excludedCommands` does **not** exempt them:
a bare foreground `cargo check --workspace --features vec` fails identically.

CI for these phases therefore runs as

```
env -u RUSTC_WRAPPER CARGO_BUILD_RUSTC_WRAPPER= otto ci
```

which disables the wrapper from both places it is configured (`.zshenv:63`
and `~/.cargo/config.toml`'s `build.rustc-wrapper`) and compiles uncached.
Turning the Bash sandbox off for the session (`/sandbox`) restores the cache.

## Phase 2: Shared tag classifier, unwired

### Design decisions

- **`TagClassifier` stayed synchronous, so `FabricClosedVocab` got its own sync port** (`distillers/src/tags.rs`, `FabricRunner` + `ShellFabricRunner`). The doc's API Design says the trait is synchronous *and* that the Fabric impl "uses the `FabricShell` adapter", but `FabricCaller::call` is `#[async_trait]` (`distillers/src/fabric.rs:25`), so the two cannot both hold. Sync wins on the evidence: cortex's `classify_note` is a sync `fn` (`cortex/src/classify.rs:653`) and cortex deliberately has no tokio runtime in that path (`cortex/src/llm.rs` uses blocking `ureq` for exactly this reason). Making the trait async would force a runtime into cortex; making the Fabric impl `block_on` an async caller from a sync trait risks deadlocking inside borg's runtime. `ShellFabricRunner` therefore calls `vault::fabric::run_pattern`, the same sync helper `FabricShell` wraps in `spawn_blocking`. Borg wraps the sync classify at its own async call site in P6.
- **`build()` takes the `TagMapping` too.** The doc's signature is `build(cfg, canon, fabric)`, but `match_to_canonical` needs the mapping, and tier 0 (the mapping hit) is what makes `Deterministic` report `High`. Signature is `build(cfg, canon, mapping, fabric)`, with `build_fallback` beside it for the configured error path both call sites need.
- **`Deterministic` ranks by tier, and reports no scores.** The invariant list says output is "sorted by score descending then name", but `Deterministic` has no scores. It sorts by matcher tier ascending then name, which is the ordering `filter_and_cap` already applies, and returns `scores: None` rather than inventing numbers. `rank_and_cap` is the single place the score-ordering invariant is enforced for the impls that do have scores.
- **`merge_shard_scores` takes the max when a label appears in more than one shard.** It cannot happen with a partition, but the function is public and the merge must be total; taking the max keeps it monotone.
- **`ClassifierDev::finish` re-filters against `CanonicalSet.all` after selection.** The labels we send *are* the vocabulary, so this is redundant against a well-behaved service. It is there because the subset invariant is a vault-correctness property and must not depend on a remote service echoing back only what it was given.
- **`build_kind` degrades `fabric-closed` to `deterministic` when no runner is supplied**, rather than returning a classifier that errors on every call. A host with no Fabric wiring should tag deterministically, not produce a stream of degraded receipts.
- **The recorded fixture is the P1 117-label two-shard shape**, not the 100-label Phase 0 capture (`distillers/src/tags/fixtures/classifier-dev-2026-09-20.json`). Scores are the ones recorded in the design doc's Phase 0 entry, placed in whichever shard the sorted even split puts them in. Each shard's `scores` map carries only the labels the Phase 0 record preserved; omitted labels scored below the recorded sub-cap floor, which is all the merge needs. The tests need the production shard shape, and a 100-label fixture could not exercise the merge at all.

### Deviations

- **`FabricClosedVocab` does not use `FabricCaller`.** See the first design decision. The doc's `fabric: Option<&dyn FabricCaller>` parameter is `Option<Arc<dyn FabricRunner>>`.
- **Two extra tests beyond the doc's list**: `retag_never_writes_empty` (the API Design states the rule but the doc's P2 criteria did not name a test for it) and `segment_guard_holds_through_the_classifier` (proves P1's `no-segment-match` survives the new seam, since every impl now routes candidates through `match_to_canonical`).

### Tradeoffs

- **Whole-batch failure on any shard error, as specified.** The alternative, returning the tags from the shards that did answer, produces a set that looks complete but was ranked against part of the vocabulary. Failing the batch makes the outage visible through the `degraded=true` receipt instead.
- **`ClassifierDev::classify` is `classify_batch` with one element** rather than a separate code path, so the single-note and daemon-batch paths cannot drift in how they shard, merge, threshold, or cap.

### Open questions

- None.

### Deferred operator steps (not run by this phase)

- None. Phase 2 is unwired by design: nothing calls the classifier until P6 (borg) and P7 (cortex), and no config file references it yet.
