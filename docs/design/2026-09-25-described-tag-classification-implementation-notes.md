# Described Tag Classification: Implementation Notes

Design doc: `docs/design/2026-09-25-described-tag-classification.md`

## Phase 0: Measure on production input against a gold set

### Design decisions
- Gold labels (`~/Claude/tags-only-trial/gold.md`): Claude ticked the set and Scott reviewed and accepted it. G2 now records that provenance.
- Wire text (`p0/build.py`): a port of `vault::search::parse_body_summary` plus `body_excerpt`. The title comes from frontmatter `title:`, falling back to the filename.
- Protect-list tags vs. domains: every note carries its domain tag. The `resources` domain is not a tag, so those notes get `entertainment`, `finance` or `books` instead.
- A2-A4 inputs are unclipped (A0 won A0 vs. A1), per "run on whichever of their inputs wins". P1 still ships the 900 clip.
- A4 indicator labels (`p0/arms.py:INDICATORS`): 3 per protect-list tag; the tag's score is the max of its indicators. The indicators are sent without descriptions.
- Screen query embedding: `p0/bge.py`, a numpy forward pass of the cached `BAAI/bge-small-en-v1.5` weights with CLS pooling and L2 norm, as in `vault/src/embedding/candle.rs:380`. It reproduces the stored oracle.db summary vectors at cosine 1.0 (4 of 4 checked). This avoids new repo code and a Rust build.
- The BM25 union takes every phrase hit, not just the top N, so the candidate set can exceed N (for example, `work` matches 1,076 notes).

### Deviations
- Descriptions were drafted from sample-note titles (up to 8 per tag, newest first, the 53 excluded), not from 5-10 full notes. Tags with no samples (`entertainment`, `politics`, `nix`, `synth`) were drafted from the slug alone.
- The drafter had already labeled the gold set, so A2-A4 carry leakage beyond "samples exclude the 53". This is recorded in Phase 0 Results; P5's confirmation on `gold-2` is the control.
- One A4 call's response was truncated in transit (`http.client.IncompleteRead`), so it was never logged, and its cost (about $0.0065) is missing from `calls.jsonl`. `jev.py` now retries transport errors as code 599.
- A6 was first run with L2 1e-2 over 300 iterations, which underfit (F1 0.016). The recorded run is the vectorized refit (L2 1e-4, 2,000 iterations, learning rate 20).

### Tradeoffs
- The instability metric reports both note-level (set differs) and label-level (symmetric difference over union) rates. The doc's "pairwise disagreement rate" is read as note-level for the gates.
- The bootstrap compares mean-over-repeats F1 between arms, rather than pairing single calls, so arms with 3 and 6 repeats compare like for like.
- Cost model: fitted in the doc's form. It is not identifiable with a constant 53-note chunk, so the per-call token and USD figures are given beside it.

### Open questions
- P3-P5: A2 misses "beats baseline" by 0.004 F1 (CI lower bound -0.006), while lifting protect-list recall (work 1/7 -> 4/7). Go on the near-miss, or hold to the rule?
- P6 passes on the baseline's six calls by a small margin (0.195 -> 0.170). Ship `repeats: 3` at 3x the call cost?

## Phase 1: Classifier infrastructure

### Design decisions
- One clip, at the wire: `clip_classifier_text` (`distillers/src/tags.rs`) replaces `body_excerpt`, which is deleted rather than kept as a shim. `TagSources::from_body` (`borg/src/pipeline/tags.rs`) and `cortex::classify::classifier_text` now hold the body whole and leave the clip to the classifiers.
- `FabricClosedVocab::prompt` also goes through `clip_classifier_text`, because `from_body` no longer clips. Without that, the fabric-closed path would send a whole transcript to Fabric, reopening audit M3. The prompt's `TITLE:` line is folded into `TEXT:`, since the clipped wire text already leads with the title.
- The HTTP boundary is a private `Transport` port (`send(body, token) -> (status, body)`), with `UreqTransport` in production. Status handling, parsing, and `ensure_jev` stay in `ClassifierDev::post`, so the fake responder exercises the same path production does.
- Batch sizing (`plan_chunks`, `note_tokens`): a greedy chunk stays under `REQUEST_TOKEN_BUDGET` = 157,708 tokens (the largest passing P0 call) and under `MAX_TEXTS_PER_CALL`. The per-note estimate is the doc's model: 26.9 + 0.43 x wire chars + 24.7 x labels sent + 0.21 x total label chars. On a 59-label bare shard with the 715-char mean P0 wire text, that is about 1,900 tokens per note (P0 measured 100,663 / 53 = 1,899), so a chunk holds about 83 notes. That is under the 159 that 402'd and over the 53 that passed.
- The 402 split only happens on `402` whose body contains `request_spending_limit`. Any other 402, such as the day cap, fails the chunk as it stands and is not retried, which is P2's backstop rule.
- A note that failed one shard is not sent to the next, so a failed note is not paid for twice.
- `CallUsage` rows cover every HTTP call. A refused or failed call is recorded as `ok: false` with 0 tokens and $0, since no response reported a cost for it.
- `sb doctor`: new `classifier_settings_finding` prints one info line each for borg and cortex. The live probe (`classifier_probe_findings`) still runs on borg's config, as before.

### Deviations
- `score_batch`'s signature matches the doc. `finish` now takes the merged `&HashMap<String, f32>` for one note instead of the per-shard slice, and `merge_shard_scores` does the merge inside `score_batch`. Same effect, correct seam.
- `ensure_jev` still fails the whole call when any result is non-Jev, rather than failing per note. The notes in that chunk then get `Err` (per note, as the doc requires), but one non-Jev answer fails its chunk-mates too. I kept this fail-closed rule unchanged because the doc does not ask for a change.
- The "later repeat failed" half of success criterion 3 cannot be tested yet: repeats are P6. The shard half is tested.

### Tradeoffs
- The token budget is the measured pass (157,708) rather than an extrapolation toward the $0.01 cap, which would be about 238k tokens at $0.042/M. The measured number costs a few more calls, but it never plans a chunk P0 did not see pass, and the 402 split still covers drift.
- `Transport` is crate-private. P2/P8 tests in cortex may need a fake across crates. Widening it then is one `pub`; exposing it now would be work for a later phase.

### Open questions
- None.

## Phase 2: Spend ledger and resumable bulk runs

### Design decisions
- The ledger and day allowance live in `distillers::tags::spend` (`SpendLedger`, `DaySpend`, `Allowance`, the `Clock` port). The resumable loop lives in `distillers::tags::bulk` (`BulkRun::run`, `ScoreCursor`). Both are submodules of `tags`, so their tests reach the private `Transport` and `ClassifierDev::with_transport` with no widening.
- The ledger path is `vault::paths::classifier_spend_ledger()` (`~/.local/share/sb/classifier-spend.jsonl`). It sits under `sb/`, not a subsystem's directory, because every bulk path and `sb doctor` share one IP-wide day.
- Rows are written only for calls with `ok: true`, from the response's `pricing.estimated_usd`. A refused or failed call carries no pricing, so it has nothing to record. A ledger line that does not parse is an error (`SpendLedger::day_total`), because reading it as low spend would let a run spend into the cap.
- The generic cursor is `ScoreCursor { scoring_config_hash, scores: {id: {scores, wire_text_hash}}, failed }`. P8 adds `tag`, `candidate_mode`, and `candidates` around it and owns the file path. `BulkRun::run` takes a `persist` callback rather than a path, and `ScoreCursor::save` writes atomically. `keep` limits the slugs stored per note, since P8 needs only the target slug.
- `scoring_config_hash` (`ClassifierDev::scoring_config_hash`) is SHA-256 over the endpoint, the shards exactly as sent, `max_labels`, and `CLASSIFIER_TEXT_CHARS`. P5 (instructions, described labels) and P6 (repeats) extend it. `wire_text_hash` is SHA-256 of `clip_classifier_text`. I used sha2 over std's `DefaultHasher` because the digest must survive a toolchain upgrade. `sha2 = "0.10"` is the version borg already builds.
- Run chunks (`plan_run_chunks`) are planned against the shard whose labels cost the most per note. So each run chunk is one request per shard, and "a chunk" costs one call per shard.
- The stop rule: before each chunk, the prediction is the model's estimate scaled up (never down) by what the service has charged against the model so far in this run. Spend counted against the limit is, per chunk, the larger of the reported and estimated cost. A response that under-reports, or has no `pricing`, cannot run the day dry. This is also what makes the $0.02-per-chunk fake stop after two chunks at $0.05.
- Service limit: `CallOutcome::ServiceLimit` covers any 429 and any 402 that is not `request_spending_limit`. `score_batch` sends nothing after one and reports it in `ScoredBatch::service_limit`. Notes it left unscored stay pending in the cursor, not failed.
- A multi-note `--retag` is a bulk path: `Classifiers::classify_bulk` (`cortex/src/classify.rs`). The primary's `estimate` is checked against the allowance, and the run refuses above it, naming how many notes fit. The estimate line is reported as an Info row against the ledger path. The batch then goes through `classify_batch_metered`, and every successful call is written as `path: "retag"`. `TagClassifier` gains two defaulted methods, `estimate` (`None`) and `classify_batch_metered` (no calls), so Deterministic and Fabric retags are unchanged and borg is untouched.
- A retag note left unscored at classifier.dev's limit is left unchanged and reported as a Warning. It does not get the deterministic fallback written over it.
- `sb doctor`: the settings line for each subsystem now shows `daily-usd-budget` and `ingest-reserve-usd`. A new `classifier_spend_finding` prints today's ledger total and the allowance, read with cortex's keys since bulk runs are cortex's, and warns on an unreadable ledger.
- Both config templates document the two keys, commented out, since code defaults carry the values.

### Deviations
- `daily-usd-budget` and `ingest-reserve-usd` are `f64`, not the doc's `f32`. `CallUsage.estimated_usd` and the ledger are `f64`, and mixing in `0.1f32` noise would put comparisons at the allowance boundary off by rounding.
- `--retag` has no `--max-usd`: the doc lists that flag only for `tag-apply`. `BulkRun` takes `max_usd` for P8.
- `--retag` classified one note per call before this phase. A multi-note retag now batches through `classify_batch_metered`, so it can be metered and written to the ledger. As P1 recorded, one non-Jev answer now fails its chunk-mates (they get the fallback), where a per-note call would have failed only itself.
- `lint_classify` now returns `Result<Report>`, because a dry-run retag also spends and must be able to refuse.

### Tradeoffs
- Scale the next chunk's prediction by what the service charged, vs. trusting the token model alone. The model does not know what the service will charge, and the success criterion's fake charges far more than the model predicts. Scaling only upward keeps the rule fail-closed.
- `failed` is a report, not a skip list: the next run retries failed notes, vs. skipping them for good. A transient 500 should not strand a note, and a note that keeps failing costs nothing, because failed calls are not billed.
- The retag estimate is checked once, up front, against the whole set, vs. scoring until the allowance runs out as the cursor does. `--retag` has no cursor to resume from, so a partial retag would leave a set half-rewritten with no record. The doc asks for refusal.
- Cortex's daemon classify (inbox and catch-up) is not treated as a bulk path and writes no ledger rows. The doc names only `--retag` over more than one note, and `tag-apply`. See Open questions.

### Open questions
- Cortex's daemon classify pass also calls classifier.dev once per inbox or catch-up note when `classifier: classifier-dev`, and it writes no ledger rows. That spend comes out of `ingest-reserve-usd` alongside borg's. Should it be metered too?
- Not run live: no `--retag` over several notes against classifier.dev, and no `sb doctor` on a host. Both were exercised only through fakes. A live check after deploy: `sb cortex classify --retag 'notes/a*.md'` (dry run) should print the estimate row, and `sb doctor` should show the spend line.

## Phase 3: Descriptions in the vocabulary schema

### Design decisions
- Gate: under the doc's rule neither A2 nor A3 beat baseline in P0. Scott overrode the rule on 2026-09-26 ("build it"; see Resolved Decisions and Phase 0 Results), so this phase is built on that decision. Descriptions are still not sent to classifier.dev: that is P5, which stays gated on the `gold-2` confirmation run.
- `TagEntry { slug, description: Option<String> }` (`vault/src/canonical.rs`) deserializes through `#[serde(try_from = "RawTagEntry")]`, where `RawTagEntry` is the untagged enum `Bare(String) | Described(serde_yaml::Mapping)`. The entry rules run in `TryFrom`, so the raw `serde_yaml::from_str` in `promote_tags` enforces them. The errors carry serde_yaml's path and location (`tags.tech: tag "go": description contains ... at line 3 column 5`).
- The map arm reads a `serde_yaml::Mapping`, not a `HashMap<String, String>`, so a non-string value (`- rust: 0.9`, `- rust:`) gets its own message naming the tag, not serde's "did not match any variant".
- `CanonicalSet.descriptions` (slug -> description) holds described tags only. `all` still holds every slug, so every reader of `all_tags()` / `canonical_set()` compiles unchanged. Test literals of `CanonicalSet` gained `descriptions: Default::default()`.
- `TagEntry::yaml_line` is the one writer of an entry's text: `slug` or `slug: "description"`, with `\` escaped. A validated description holds no `"` and no control character, so that escape is all a double-quoted scalar needs to read back the same string. `yaml_line_reads_back_as_the_same_entry` pins this.
- `insert_tags_into_group` now takes `&[TagEntry]` and writes each through `yaml_line`. `promote_tags` passes bare entries, so its output is unchanged until P7.
- `describe_entry_in_group(text, group, slug, description)` (`cortex/src/proposals.rs`) is `pub` for P4 and has no production caller yet. It parses each entry line of the group as a `TagEntry`. It rewrites the target line only if that line reads exactly as `yaml_line` would write it. So a trailing comment or a single-quoted form bails instead of being dropped or reformatted. It returns the buffer; the caller validates the whole file before writing, as `promote_tags` does.
- `tag-values.md` renders `| Tag | Description |`. An undescribed tag gets an empty cell, and a `|` in a description is escaped as `\|`. `load_canonical_tags` (`sb/src/cli/cortex.rs`) returns `CanonicalTagsFile::sorted_entries()`, sorted by slug as before. `render_all` / `render_all_at` take `&[TagEntry]`. oracle's own `load_canonical_tags` is untouched, since it lists slugs only.
- `sb doctor`: `tag_description_finding` (`sb/src/cli/checks.rs`) prints an Info line in the classifier section, `N of M tags have no description`. It reads the vocabulary at cortex's `sweep.canonical_path`. It is Info, not Warn, because an undescribed tag still classifies.

### Deviations
- The doc says "`TagEntry` untagged enum". The public `TagEntry` is a validated struct, and the untagged enum is the private raw form it is converted from. Same effect, correct seam: consumers read `slug` and `description` without matching two variants.
- Beyond the doc's three entry rules (one key, at most 200 chars, no `"`), parsing also refuses an empty description and one containing a control character (a line break or tab). An empty one would reach Jev as `slug: `. A line break would split the label's line both in the file and in the Jev question.
- The success criterion `sb cortex schema --render` ran with the worktree binary (`target/debug/sb cortex --vault <scratch copy> schema --render`) against a scratch copy of the live `system/schemas/`, not against `~/repos/scottidler/obsidian`. Result: exit 0, `tag-values.md` written with `| Tag | Description |` and 117 rows with empty description cells; the other three docs were `unchanged`.

### Tradeoffs
- Row format `| slug |  |` (empty cell) vs. omitting undescribed tags or writing `-`. The empty cell keeps every tag listed and makes the backfill gap visible in the vault. A placeholder would read as a description.
- The describe rewrite bails on any line not in the written form, vs. rewriting it anyway. A hand-formatted line (comment, single quotes) is rare, and silently losing a comment or re-quoting Scott's text is worse than a refusal naming the line.

### Open questions
- The live vault's `system/schemas/tag-values.md` still has the one-column table. After this ships, `sb doctor` and `sb cortex schema --check` report it as drifted until someone runs `sb cortex schema --render` on the host. Run it as part of the deploy?
- Not run live: `sb doctor` on a host (the finding is unit-tested, and it is checked against the shipped file).

## Phase 4: sb cortex tag-describe

### Design decisions
- Gate: as P3. Under the doc's rule neither A2 nor A3 beat baseline in P0. Scott overrode the rule on 2026-09-26 ("build it"; see Resolved Decisions), so this phase is built on that decision. No description was written into `config/canonical-tags.yml`: the backfill `--apply` is Scott's approval step.
- Drafting pattern (`borg/patterns/describe-tag.md`), on Scott's steer: Phase 0's trial descriptions were thin (mean 73 chars, drafted from titles only, 3 of 117 with a `; not ...` clause). The pattern drafts from each sample's title and summary, aims for 120-200 chars, names concrete terms that appear in the samples, and adds `; not <neighbor>` whenever a sibling or a common homonym collides (worked example: football blocking; not ad blockers). It stays a definition, never a rule about when to apply the tag, because P0's instruction-style arm A3 cut recall. Registered in `PATTERNS` (`sb/src/cli/bootstrap.rs`).
- Module `cortex::tag_describe` (`cortex/src/tag_describe.rs`): `draft_descriptions` (dry run) and `apply_descriptions` (`--apply`) are separate functions, so `--apply` has no drafter in reach and cannot make a fresh LLM call. `run_draft` is the production entry: it scans the vault and builds the `FabricDrafter`.
- The drafter is a port, `TagDrafter::draft(&DescribeRequest) -> Result<String>`. `FabricDrafter` runs the pattern through `crate::fabric::run_pattern` with `fabric.timeout-secs`, the same runner the entities extractor uses. `parse_description` reads `{"description": ...}` (a code fence or prose around the object is tolerated) and holds it to `TagEntry::described`'s rules. An over-cap or quote-bearing answer is an error, never trimmed to fit.
- The pattern's input is the tag, the group, every sibling entry as `TagEntry::yaml_line` writes it (so a described sibling shows its description to contrast against), and the samples. Each sample is `clip_classifier_text(title, summary)`, where summary is `classify::classifier_text` (`## Summary`, else the body), which is now `pub(crate)` along with `note_title`. So the drafter reads the same string the classifier scores.
- Samples (`select_samples`): notes whose `tags` carry the slug, minus the `--exclude-from` paths, newest first by `date` then `ingested`, ties broken by path so a re-run picks the same set. `run_draft` first drops notes the vault config does not govern (`vault.exclude` / `vault.include`, via `crate::is_excluded`), which keeps `system/quarantine/` copies out of the samples. Default 8 (`DEFAULT_SAMPLES`, Phase 0's count), `--samples` 1-10.
- `--exclude-from` (`load_exclude_list`): one vault-relative path per line, blank lines skipped, a leading `./` stripped. An absolute path is an error naming the line, because it would match no note and silently exclude nothing.
- The cache is `vault::paths::tag_describe_cache_dir()` (`~/.cache/sb/tag-describe/<tag>.json`, new `xdg_cache_dir()` honoring `$XDG_CACHE_HOME`). A `CachedDraft` is `{tag, group, description, source: fabric | set, samples: [path]}`. `samples` is what the P5 harness checks against both gold sets. On load, the tag must match the file name and the description is re-validated.
- A dry run proves each draft lands before caching it: it runs `describe_entry_in_group` on the in-memory buffer. A layout the rewrite refuses shows up at review time, not at `--apply`.
- Failure scope: an unknown tag, or a protect-list tag without leave, fails the whole run before any LLM call. A single bad draft (fabric error, unparseable or rule-breaking answer) fails only that tag. It prints as `! tag: <error>`, is not cached, and the command exits non-zero after the rest are drafted. For a backfill over about 112 tags, one bad answer does not throw away the others.
- `--apply` is all-or-nothing: every tag's text (a cached draft or `--set`) is gathered first, so a missing cache refuses the run before any write. The edits go into one buffer, which is parsed as a whole `CanonicalTagsFile`, then written with one atomic write. The deployed-copy refusal is now a shared `refuse_deployed_apply` in `sb/src/cli/cortex.rs`, used by both `tag-promote` and `tag-describe`.
- Output: `+ tag: "description"` per tag, then an indented line with the sample count (or `--set wording`), the group, and the cache path.

### Deviations
- `--set TAG=TEXT` is exempt from the protect-list refusal. The doc refuses protect-list tags without `--allow-protected` because "their wording should be his", and `--set` is his wording. What `--allow-protected` gates is an LLM draft, at both dry run and `--apply` (a cached `fabric` draft of a protected tag still needs the flag to land).
- `--set` follows the command's dry-run default rather than writing at once: without `--apply` it prints and caches the wording (`source: set`, no samples); with `--apply` it writes. The doc's "writes Scott's own wording with no LLM call" holds for `--set ... --apply`. Either way, no LLM call is made for a `--set` tag. Positional tags are optional when `--set` is given.
- Live success criteria were run with the worktree binary against a scratch `XDG_CONFIG_HOME` (a copy of the host `cortex.yml` plus the new `describe-tag.md`, since the deployed patterns dir does not have it yet) and a scratch `XDG_CACHE_HOME`, so no draft was left in the host cache. `MISE_CONFIG_DIR=~/.config/mise` was needed because the fabric binary is a mise shim, and mise refuses the global config as untrusted once `XDG_CONFIG_HOME` moves. The vault read was the live vault, read-only; `--exclude-from ~/Claude/tags-only-trial/gold-exclude.txt` (83 paths).
  - `sb cortex tag-describe rust --canonical config/canonical-tags.yml --exclude-from ~/Claude/tags-only-trial/gold-exclude.txt`: exit 0, one line `+ rust: "the rust programming language: crates, cargo, memory safety, compiler, rewriting codebases (typescript/zig to rust), rust binaries and cli tools; not corrosion or rust the metal oxide"` (183 chars), drafted from 8 samples, none in the gold list; `sha256sum -c` on `config/canonical-tags.yml` OK (byte-identical).
  - `sb cortex tag-describe work --canonical config/canonical-tags.yml`: exit 1, `Error: tag "work" is on the protect list (`no-classifier-tags` in .../config/canonical-tags.yml); write its description yourself with --set work=TEXT, or pass --allow-protected to use a drafted one`. The file is byte-identical and no LLM call was made.
  - `--apply` was checked against a scratch copy of the vocabulary only: `rust`'s line became `    - rust: "..."` and `diff` showed no other change. `--apply` for `python`, which has no cache, refused with `no cached draft for "python" ...`.

### Tradeoffs
- Keep drafting the remaining tags after one fails and exit non-zero, vs. stopping at the first failure. Every good draft is cached and reviewable, and the failure is still loud (a `!` line and the exit code).
- Newest by `date` (the content date borg preserves across reingest) before `ingested`, vs. `ingested` first. `date` is what a reader means by a note's age, and it is present on manual notes that have no `ingested`.
- One cached file per tag, overwritten by each dry run, vs. keeping a history of drafts. `--apply` must land exactly the last draft shown; a history would add a choice with no command to make it.

### Open questions
- The first `otto deploy` after this ships installs `describe-tag.md` into `~/.config/sb/patterns/`. Until then, a host run of `tag-describe` fails with fabric's missing-pattern error, and `sb doctor`'s pattern drift check flags the new file.
- Tags that no note outside the gold sets carries: 4 were drafted from the slug alone in P0 (`entertainment`, `politics`, `nix`, `synth`). The command drafts them from the name, group and siblings, and prints `drafted from 0 sample note(s)`. Should the backfill `--set` those by hand instead?
