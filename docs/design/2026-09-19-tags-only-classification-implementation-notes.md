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

## Phase 3: Reingest preserves tags

### Design decisions

- **`FieldValue` is generic over the preserve keys, not special-cased to `tags`** (`borg/src/pipeline/publish.rs`). `read_cortex_fields` decides list-vs-scalar from the on-disk shape, so `cortex-quality-issues: [no-outbound-links]` now parses as `List(["no-outbound-links"])` rather than the raw string `"[no-outbound-links]"`. It round-trips identically either way; making the parser shape-driven avoids a key-name special case that the next list-valued cortex key would have to remember to add itself to.
- **`remove_key` replaces the old single-line `retain`** (`borg/src/pipeline/atomic.rs`). The previous code removed only the `key:` line, which for a block list left its `- item` bullets orphaned under whatever key followed. `remove_key` removes the key *and* its list body in either form, and returns the value it removed, which is how `apply_cortex_fields` gets the freshly rendered tags to union against. `block_list_bullets_are_not_orphaned_on_replace` pins it.
- **The merged list is always written in block form.** `render_note` and `Frontmatter::to_yaml` already write block; cortex switches in P4. Writing block here means a reingest of a pre-P4 inline note normalizes it on the way through, which is the same direction P4 takes the vault.
- **`max_per_note` is `usize::MAX` when no vocabulary is loaded** (`borg/src/pipeline.rs`). The cap has to come from the loaded `CanonicalSet`; if there is none, the honest behavior is not to truncate. Inventing a default would silently drop a preserved tag on a host whose vocabulary failed to load.
- **The session path captures `tags` rather than carrying it** (`borg/src/pipeline/session.rs`). `PriorFrontmatter` gains a `tags` field beside `status`, the existing precedent for a `RENDER_NOTE_KEYS` key that a replace does not simply rewrite. The union happens at the call site, where the fresh list exists; the test asserts `tags` is NOT in `prior.carried`, since being carried verbatim would bypass the merge entirely.

### Deviations

- **The URL-path fail-closed change alters an existing contract deliberately.** `read_cortex_fields` returned `Vec` and swallowed read errors; it now returns `Result` and propagates. The doc calls for this (P3: "a preserve read failure fails closed on both paths"), but it means a reingest of a note that became unreadable between resolve and publish now fails the trace instead of publishing a stripped note. That is the intent.
- **`test_read_cortex_fields_none_present` was renamed, not deleted.** Its fixture carries `tags:\n  - rust`, which was "no preserve keys present" before P3 and is now exactly one. It is `test_read_cortex_fields_reads_a_block_tag_list` and asserts the new answer.
- **`borg_owned_key_policy_matches_the_declaration` gained two asserts** rather than being rewritten: `tags` is in the owned set, and `tags` is in `CORTEX_PRESERVE_KEYS`. The second is the cross-path guard: if a later change removes it from one list and not the other, the two reingest paths silently disagree about whether a tag survives.

### Tradeoffs

- **A note at the cap keeps its preserved tags and drops the fresh ones**, as the doc specifies. The alternative (fresh wins at the cap) would let a refetch silently rewrite classification, which is the failure the phase exists to prevent. Logged at info, and `--retag` is the explicit refresh.
- **`unquote` strips quotes without a YAML parse.** A real parse would reject notes the rest of the pipeline tolerates, and this runs on a frontmatter line, not arbitrary YAML. The narrow version handles the quoted forms actually found in the vault.

### Open questions

- None.

### Deferred operator steps (not run by this phase)

- None in this phase, but the ordering matters: P3 must be deployed (`otto deploy`) before P4's migration runs, or a URL reingest in the window between them drops a freshly migrated tag. The design doc's Rollout step 2 says the same.

## Phase 4: Domain-as-tag migration

### Design decisions

- **Block-list emission moved into `scope::insert_frontmatter_fields`, not into `replace_tags_in_frontmatter`** (`cortex/src/scope.rs`). A `Value::Sequence` of scalars now emits `key:` plus indented `  - item` lines; serde_yaml would have emitted its bullets at column 0, a *third* spelling of the same list. Fixing it at the one place that serializes sequences means any other caller passing a list also gets the vault's form, and `replace_tags_in_frontmatter` becomes a two-line call that passes a sequence.
- **`field-to-tags` normalizes through `Domain::from_str` first, then `hygiene::normalize_domain`** (`cortex/src/migrate.rs::field_to_tag_value`). See the deviation below: the doc's stated normalizer does not carry the `knowledge -> life` alias.
- **Tag transforms run BEFORE field transforms in `apply_migrate_selected`.** A single migration could legally carry both `field-to-tags: {domain: ...}` and `field-drops: [domain]`; if the drop ran first the value would be gone before it was copied. The phase ordering makes that combination safe even though this doc splits it across P4 and P11.
- **The transform rewrites every visited note's tag block in canonical block form**, which is how a pre-P4 inline list gets normalized on the way through. Idempotent: a value already present is not appended twice, so a second `--apply` writes zero files (`field_to_tags_is_idempotent`).
- **`--only` errors when the name matches nothing**, listing the available names, rather than silently running zero migrations. A typo in the operator runbook should fail loudly, not look like a clean no-op.
- **`load_plan` is public and tested against the shipped undo file** (`load_plan_reads_the_shipped_undo_file`), which also asserts the inverse carries no `field-to-tags`. That is the guard against someone "fixing" the inverse into a round-trip it cannot be.
- **The cap for the dry-run's over-cap warning is read from `config.sweep.canonical_path`, best-effort.** An unreadable vocabulary file means no cap column, not a failed dry-run.

### Deviations

- **The design doc is wrong about which function maps `knowledge` to `life`.** It says `field-to-tags` "normalizes through `hygiene::normalize_domain` (`knowledge` -> `life`)". `normalize_domain` handles emoji folder paths and case only; the `knowledge -> life` backwards-compat alias lives in `Domain::from_str` (`vault/src/schema.rs:117`). Caught by `field_to_tags_strips_quotes_and_normalizes`, which failed with `["knowledge"]`. The implementation runs the hygiene pass, then a schema parse, and falls back to the hygiene result for anything the schema rejects.
- **The dry-run does not list "notes that already carried two or more domain-name tags before the run."** The doc asks for it, but a per-note lint has no access to the set of *all* domain values the migration could produce, only to this note's own. A first attempt at approximating it was meaningless and was removed rather than shipped as a misleading count. The over-cap warning, which is the part that changes an operator decision, IS implemented.
- **`lint_migrate` / `apply_migrate` keep their `&[MigrationConfig]` signatures** and delegate to new `*_selected` variants taking `&[&MigrationConfig]`. Existing tests and callers are untouched; `--only` narrows by reference rather than cloning configs.

### Tradeoffs

- **Four existing tests changed their assertions**, because the inline-to-block writer switch is exactly what this phase does: `test_replace_tags_in_frontmatter`, the two `..._does_not_orphan_bullets` regressions, and `rewrite_note_tags_returns_true_and_writes_when_frontmatter_present`. The two orphan-bullet tests could no longer assert "no `- ` lines exist" (block form has its own), so they now assert via a shared `assert_no_orphan_bullets` helper that every bullet sits under a key that opened a list. That keeps the original regression pinned instead of weakening it to nothing.
- **`tags-remove` is a blunt instrument, documented as such in the plan file itself.** It cannot distinguish a migrated `football` from one of the 125 that predate the migration. The obsidian commit is the authoritative undo and the file says so at the top.

### Open questions

- None.

### Deferred operator steps (not run by this phase)

The apply writes the live vault and is NOT run here. In order:

1. dotfiles `~/.config/sb/cortex.yml`: add the `v5-domain-as-tag` migration entry:
   ```yaml
   migrations:
     - name: v5-domain-as-tag
       field-to-tags:
         domain:
           exclude: [resources, system]
   ```
2. `otto deploy` (also picks up P1's config files and P3's preserve behavior, both of which must be live before the migration runs).
3. `systemctl --user stop borg cortex`. Cortex's per-tick sweep and lint write the same files; borg snapshots preserved fields minutes before it applies them, so an in-flight reingest straddling the migration would write back a pre-migration snapshot.
4. `sb cortex migrate --only v5-domain-as-tag` (dry-run first; expect ~2,545 files and a list of any note that would exceed `max-per-note: 8`).
5. `sb cortex migrate --only v5-domain-as-tag --apply`.
6. `find . -name '*.sync-conflict*' | wc -l` == 0 in the vault.
7. `systemctl --user start borg cortex`, then one daemon tick, then confirm the ten domain tags survived it.
8. Commit the obsidian repo. **This commit is the authoritative undo for the whole migration**; the inverse plan file is not exact.

### Executed 2026-09-20 (the operator steps above were run, not deferred)

Scott's instruction mid-phase: "there is no my-side". The whole sequence ran.

- dotfiles `cortex.yml`: the three `ai-llm` aliases removed and the
  `v5-domain-as-tag` entry added, committed as `ed944a0`.
- `otto deploy`: the release build succeeded but **the install step failed**,
  `Read-only file system` on `~/.cargo/bin/sb`, which is outside the Bash
  sandbox's write set. This mattered later (see the daemon-tick finding).
- Vault snapshot committed FIRST as `17604101` with both daemons stopped, and
  verified clean of any `tags:` / bullet / `domain:` change before committing,
  so it is a true pre-migration undo point.
- P1's deferred criteria, now measured on the deployed config: `max-per-note:
  8`, `no-classifier-tags` present, `tech|diy|homelab: null` 3 -> 0,
  `resources|system: null` 2, `ai-llm` aliases 3 -> 0, all ten domain names
  canonical. `sb cortex sweep --migrate --dry-run`: 1 note would be rewritten
  with `drop: []`, so **0 notes lose a domain-name tag**.
- P4 dry-run: **2,536** notes (doc predicted 2,545, due-diligence re-derived
  2,541; the vault has moved since those measurements). **0** would exceed the
  cap, which is what raising `max-per-note` to 8 bought.
- Apply: 2,536 files. Second dry-run: 0. Sync conflicts: 0.
- `ai` went **1 -> 1,149**, the number the doc predicted. That single tag was
  the alias chain's fingerprint and it is gone.

### Two implementation gaps the live run exposed

Both were found by running the phase's own criterion, not by review:

1. **Form normalization was missing.** The transform only wrote notes whose tag
   SET changed, so the ~158 notes that already carried their own domain value
   kept their inline lists and the vault still had two spellings (G4 forbids
   that). Fixed: a note in scope for the migration is rewritten to block form
   even when nothing is added, guarded so `tags: []` is left alone. Idempotence
   holds, and `field_to_tags_normalizes_form_when_the_set_is_unchanged` pins it.
2. **`exclude` was over-applied.** The first fix gated form normalization on
   `field_to_tag_value(...).is_some()`, which is `None` for excluded values, so
   the 38 `resources` and 14 `system` notes kept inline lists. `exclude` means
   "do not propagate this VALUE as a tag", not "leave this note's FORM alone".
   Split into `source_field_value` (no exclusion, answers "is this note in
   scope") and `field_to_tag_value` (exclusion applied, answers "what tag").

### The daemon tick, and why it looked like data loss

After restarting the daemons, the ten tag counts appeared to collapse: `ai`
1,149 -> 139, `tech` 840 -> 627, `homelab` 48 -> 8. **Nothing was lost.**
Counting both on-disk forms showed the totals unchanged (football 223 block +
78 inline = 301, exactly the pre-tick 301). The daemons had restarted on the
**old binary**, because `otto deploy`'s install step had failed, and the old
`replace_tags_in_frontmatter` writes inline. The tick reverted the form, not
the content.

Fixed by installing the new binary over the busy inode (six `sb oracle serve`
MCP processes held it, so a plain `cp` gives `Text file busy`; copy-then-`mv`
replaces by rename and leaves them on the old inode), then re-running the
migration to restore block form: 1,440 files, all ten counts back to their
pre-tick values.

**Carry this forward:** every remaining phase's runtime verification is void if
`~/.cargo/bin/sb` is stale. `otto deploy` cannot install it from inside the
Bash sandbox.

### Criterion amended (doc defect, evidence recorded in the design doc)

P4's `grep -lE '^tags: \[' $(grep -rl '^domain:' notes work system) | wc -l`
== 0 cannot be satisfied by P4:

- `system/**` is excluded from the vault scan (`cortex.yml:14`), so `migrate`
  never visits it, and P10 owns those 10 templates. 17 files, all under
  `system/`.
- `tags: []` is already "no tags" by the doc's own Data Model. 4 files, all
  `domain: resources`.

Amended to `'^tags: \[[^]]'` over `notes work`. Observed: **0**.

## Phase 5: Tags facet in the index

Landed as two commits because the facet half was green and self-contained
while the filter threading was still in flight; keeping the tree green mattered
more than the one-commit-per-phase convention.

### Design decisions

- **`push_tags_filter` is one shared helper, not a clause repeated per query**
  (`vault/src/search/query.rs`). It takes the caller's `notes` alias because the
  correlated subquery has to join back to it, and it owns both semantics: OR is
  `EXISTS (SELECT 1 FROM note_tags ...)`, AND is a `count(DISTINCT tag) = n`
  subquery. Both go through `idx_note_tags_tag`.
- **`Some(&[])` means "no filter", not "match nothing".** A caller threading an
  unset list through several layers would otherwise get zero rows with no
  indication why.
- **Delete-then-insert in `sync_note_tags`** rather than diffing: a note carries
  at most `max-per-note` tags, so the churn is bounded and there is one code
  path instead of three.
- **The SAVEPOINT wraps `index_one`, not `index_one_inner`.** The public entry
  point is the one that must be atomic; splitting the body out keeps the
  savepoint bookkeeping in four lines instead of threaded through 200.
- **`tags = ''` normalized to `[]` at index time.** The empty string is not
  valid JSON, so `json_each` errors on it; 303 live rows had it. AC4's standing
  invariant (facet count == `json_each` count) cannot hold otherwise.

### Deviations

- **The oracle MCP request structs do NOT get `tags` in this phase.** The doc
  assigns oracle tool params to P8, and P5 names only
  `oracle/src/server/pipeline.rs::note_matches_filters`. The four oracle call
  sites that now reach the new signatures pass `None, false` with a comment
  naming P8 as the phase that wires `req.tags` through.
- **`tags_all` is a plain `bool` parameter beside `tags`,** as the doc's API
  Design specifies, rather than a filter struct. It costs two arguments at 39
  call sites; a struct would have read better but diverges from the doc and
  from how `domain`/`note_type`/`status` are already threaded.

### Tradeoffs

- **Call-site churn was taken rather than adding `_with_tags` siblings.** A
  sibling per function would have left every existing call untouched, but P11
  would then have to collapse two functions per filter surface, and the doc's
  "takes `tags` beside it" reading is the direct one.

### Open questions

- None.

### Deferred operator steps

- `sb oracle index --force` per host, to populate `note_tags` for the existing
  3,742 rows. A plain `sb oracle index` skips unchanged files and would leave
  the table empty. Not run yet.
