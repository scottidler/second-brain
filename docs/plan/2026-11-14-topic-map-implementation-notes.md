
## Phase 1 addendum (orchestrator, supersedes the borg open question above)

- The Phase 1 worker reported that `cargo tree --locked --offline -p borg -i e-means` exits 0. Re-run on `4993bbf`: it prints `error: package ID specification `e-means` did not match any packages`, exit 101, as the acceptance criterion requires. The worker was reading the `-p sb` tree, which unifies `cortex`'s `kmeans` feature onto `vault` and lists `borg` as a co-dependent of `vault`. The criterion is sound and holds; no amendment.
- `-p oracle` and `-p distillers` also exit 101 with the same message. `-p sb -i e-means` exits 0 with `vault` and `cortex` above `e-means`.

## Phase 2: Schema and storage
### Design decisions
- `topic_state` / `topic_centroids` / `note_topics` are created by `ensure_topic_schema` (`vault/src/search/topic.rs`), called from `ensure_vec_schema` BEFORE `migrate_backend_free_model_ids`. That migration probes and renames `topic_state.model_version`, so the table must exist first (a first attempt after it failed with `no such table: topic_state` on every fresh index).
- `replace_generation(&NewGeneration) -> generation` (`topic.rs`): one `BEGIN IMMEDIATE`. It checks the generation's model and dim against the active pin inside the transaction (fail closed: no generation stale at birth), deletes all three tables, inserts centroids, assignments, then the 11 state keys. Counter = stored + 1, or 1 when absent. `assigned_at` of train-time rows is `trained_at`.
- The clock stays out of storage: `assign_batch(assignments, assigned_at)` and `NewGeneration::trained_at` take unix seconds from the caller.
- `TopicState` is a raw snapshot (key/value map plus `(topic, blob length)` per centroid) read in one read transaction. `status()` and `meta()` are the single judge. Corruption is `Err`: unparseable or missing trained key, centroid count != k, topic ids not 0..k, blob length != dim*4, and trained keys or centroid rows present with no `trained_at` (never `Absent`).
- `unassigned(exclude_types: &[NoteType], limit)` takes eligibility inputs as parameters. `quality != 'low'` and not-a-tombstone (`superseded_by`) are fixed rules mirroring `stale_embedding_targets`. Returns decoded vectors and validates blob length against the active dim.
- Storage-level `clear_topics()` (keeps `generation`) and `topic_centroids()` (returns `(TopicMeta, Vec<f32>)` only when `Valid`) are here for Phase 3; `topic_state()` is the public snapshot accessor.
- `rename_model_id` renames `topic_state.model_version`, its doc comment lists it, and `legacy_model_ids_present` probes it.
- Re-exported from `vault::search` (vec-gated): `Assignment, NewGeneration, TopicMeta, TopicState, TopicStatus, UnassignedRow`.
- `exclude_types` is stored comma-joined (`NoteType::as_str`).
### Deviations
- None from the doc's behavior. Beyond its bullets: `topic_centroids()` and `clear_topics()` exist because Phase 3 needs them; `TopicMeta` / `meta()` exist so `status` and callers share one parser.
### Tradeoffs
- Raw-snapshot `TopicState` (status takes no connection, per the doc signature) vs a validated struct built at load: chose the snapshot so corrupt state surfaces from `status()` itself, the one function every caller invokes.
- Out-of-scope `note_topics` removal is NOT in `assign_batch`; the doc puts it in cortex `assign`. Phase 3 can add a small storage fn or fold it in then.
### Open questions
- Does Phase 3 want out-of-scope removal as a storage fn here (`remove_topics(&[String])` or an `out_of_scope(exclude_types)` reader)? Left for Phase 3 rather than guessed.

Bite check for the injected-failure test: I inserted `tx.commit()` plus a fresh transaction right after the three DELETEs in `replace_generation`. The test failed with `state, centroids and note_topics are unchanged` (left `([], [], [])`, right the full generation 1). Restored; 17/17 topic tests pass.

## Phase 3: cortex::topics, the verb, the config
### Design decisions
- Out-of-scope removal is a new storage fn, `SearchIndex::remove_out_of_scope_topics(exclude_types)` (`vault/src/search/topic.rs`), one `BEGIN IMMEDIATE` DELETE of every `note_topics` row whose note is no longer eligible or holds no active-model summary row. Called by `cortex::topics::assign_index` before assigning, and only when the generation is `Valid`. Keeps SQL in vault (orchestrator ruling).
- Two more storage readers were needed and live beside it: `in_scope_summaries(exclude_types)` (the training set; `unassigned` only returns rows with no current topic, so a retrain over an existing generation could not use it) and `topic_scope(exclude_types) -> TopicScope` (eligible / in-scope / assigned counts, mean assigned cosine, and the eligible notes with no active-model summary row, from one read transaction). The eligibility rule is one private SQL fragment, `eligible_filter`, shared by all four scope readers.
- Pending = `TopicScope::eligible_unembedded` intersected with `stale_embedding_targets(Summary, active_model, u32::MAX)` paths (`cortex/src/topics.rs:scope_and_pending`). The embed loop's own query decides, so a nothing-to-embed sentinel or an empty summary settles a note. A note with an outdated summary row still has a row, so it is in scope, not pending.
- One `TopicsReport` type for train, assign and clear (`cortex/src/topics.rs`), built by one `report()` from the index after the write, so every verb prints the same fields. Beyond the doc's list it carries `configured-k` (always), `exclude-types-drift: {trained, configured}` (only when the sets differ), and `hint` (Absent and Stale name `sb cortex topics --train`). `status` serializes lowercase via a `serde::Serialize` derive added to `vault::search::TopicStatus` (oracle will need the same in Phase 5).
- Lock first, then open the index (`train`/`assign`/`clear`); the index-level work is in `train_index`/`assign_index`/`clear_index`, the hermetic seam the tests drive on `SearchIndex::open_memory()`. `run()` also rejects `train`+`clear` and `k`/`seed` without `train`, so a library caller cannot bypass clap's checks.
- Train refuses corrupt topic state (`status()` error) with a message naming `--clear`; clear never checks status, since it is the back-out.
- `vault::kmeans::check_row` is now public (the per-row finite/unit check `train` already ran) so `assign` applies the same rule; a bad row on train or assign is an error naming the note. `vault::kmeans::E_MEANS_VERSION` is recorded in `topic_state.e_means_version`; `kmeans::tests::e_means_version_matches_the_workspace_dependency` reads the workspace `Cargo.toml` so a dependency bump without the const fails CI.
- Assign loops `unassigned(.., 1024)` -> `nearest` -> `assign_batch` until empty; each step is its own short write transaction. `assign` uses no rayon.
- `sb cortex topics`: `--k`/`--seed` carry `requires = "train"` AND `conflicts_with = "clear"`. Without the second, `--clear --k 8` parsed: clap waives `requires` when a present arg conflicts with the required one. Found by `topics_k_and_seed_require_train_and_clear_conflicts_with_train`.
- Exit: Stale without `--train` prints the report, then returns `SilentFailure` (exit 1). Every other failure is an eyre error (exit 1). Absent is exit 0.
- Test-util helpers added in `vault/src/search/topic.rs`: `test_topic_tables()` (raw dump of all three tables, `TopicTables`) and `set_test_quality()`.
### Deviations
- Phase 2's `UnassignedRow` is renamed `SummaryVector`: it is now also the training-set row type, and "unassigned" would lie there.
- `unassigned`'s type filter changed from `n.note_type NOT IN (...)` to `COALESCE(n.note_type, '') NOT IN (...)`: a note with a NULL type is not in the exclude list, so it is eligible per the doc's definition; the old SQL silently dropped it. Same effect for every typed note.
- `--format yaml|json` added to `sb cortex topics`. The doc says "house format (yaml on a TTY, json when piped)"; the house helper (`sb/src/cli/output.rs`) and its siblings (`sb borg queue`) carry the one `--format` override, so this matches them.
- The doc names three report types (`TrainReport`, `AssignReport`, `ClearReport`); there is one, `TopicsReport`, because the three would be the same shape.
- `removed-this-run` on `--train` counts the prior generation's `note_topics` rows the swap replaced; on `--clear`, the rows cleared. The doc lists the field without defining it for those verbs.
- `daemon_tick` is not written: Phase 4 owns it and the arm, and `assign_index` is the body it will call.
### Tradeoffs
- Pending computed in cortex by intersecting with `stale_embedding_targets` (a full summary-target scan, text included) vs a new vault SQL that re-implements the target predicate: chose the scan, because the doc requires the embed loop's own test and a second copy of that predicate would drift. Cost is one query per report; only large during a migration, when train refuses anyway.
- Separate transactions for prune and assign vs one combined write: both are idempotent and run under the embed lock, so a crash between them leaves state the next run finishes. Keeps `assign_batch` unchanged.
- `TopicStatus` gained a serde derive in vault vs a cortex-local mirror enum: one status vocabulary everywhere.
### Open questions
- None.

Bite check for the pending gate: replaced `if !pending.is_empty()` with `if false && !pending.is_empty()` in `train_index`. `topics::tests::train_with_one_pending_eligible_note_errors_and_leaves_the_tables_unchanged` failed at `cortex/src/topics/tests.rs:198` (`expect_err("pending")` got `Ok`: the train ran and wrote). Restored; 18/18 `topics::tests` pass.

## Phase 4: Daemon tick and doctor
### Design decisions
- `topics::daemon_tick(config, stopped: &dyn Fn() -> bool) -> Result<TickOutcome>` (`cortex/src/topics.rs`): takes the embed lock and skips on `EmbedLockHeld` (`TickOutcome::LockHeld`), opens the index, reads status first, then `NoGeneration` (DEBUG), `Stale` (one WARN naming `sb cortex topics --train`, nothing written), or assigns. `TickOutcome` also has `Stopped` and `Assigned { generation, assigned, removed }`. The hermetic seam is `tick_index`, as for train/assign.
- Stop is a closure, not a `StopHandle`, so tests need no handle and the daemon passes `&|| stop.is_stopped()`. It is read before opening the index (before the read), before the out-of-scope removal write, and before each batch's read/write in `assign_index_until`. `assign_index` (Phase 3 callers and tests) is now a thin wrapper over it with a never-stop closure.
- Daemon arm: `topics_interval` created beside its siblings, one `select!` arm after the embed arm (before graph), `block_in_place`, calls only `daemon_tick`, logs at INFO only when a row was written or removed. daemon.rs grew 13 lines (1,247 of 1,500).
- Doctor: new `topics` section (`sb/src/cli/checks/topics.rs`) registered after `cpu`. SQL is `SearchIndex::peek_topics(db_path, exclude_types) -> Option<TopicPeek>` in `vault/src/search/topic.rs`: read-only connection, one read transaction, reuses `load_state` and `eligible_filter`, so it cannot disagree with `status()` or the eligibility rule. No file or no pin is `None`; an index without the three topic tables reads `Absent` (an old index the new binary has not opened yet), never an error. Corrupt state is an Error finding.
- Severity: Absent Info, Valid Ok, Stale Warn with fix `sb cortex topics --train`, unreadable Error. The Valid line carries generation, age, `assigned / in-scope`, and mean cosine over `assigned_at > trained_at` rows beside `train-mean-cosine`.
- Exclude types for the doctor's in-scope count come from the cortex service config.
### Deviations
- None from the doc's behavior. `TopicPeek` and `peek_topics` are additions the doctor needs to keep SQL in vault.
### Tradeoffs
- Stop closure vs `&StopHandle`: closure, for testability and because `assign_index_until` is shared with non-daemon code.
- Status checked in the tick before `assign_index_until` (which also checks it) vs relying on the inner check: the tick needs the distinct `NoGeneration`/`Stale` outcomes and the WARN; the inner check stays as the fail-closed guard for the CLI path.
- A cortex config that fails to load makes the doctor fall back to the default exclude list, since the config section already reports that failure.
### Open questions
- The tick WARNs every `interval-secs` (900s default) while Stale persists. The doc says one WARN per tick; if that is too noisy, a once-per-process latch is a small follow-up. Left as specified.

Bite check for the Stale tick test: replaced the `Stale` arm's early return with a write (`index.clear_topics()?`). `topics::tests::tick_with_a_stale_generation_writes_nothing` failed on the tables-unchanged assertion. A first mutation that only called `assign_index_until` in that arm did NOT fail the test, because `assign_index_until` has its own Stale guard (two layers); both restored.

## Phase 5: Oracle readers
### Design decisions
- Readers live in a submodule, `vault/src/search/topic/read.rs` (own `read/tests.rs`), re-exported from `vault::search`: `overview(tags, tags_all)`, `in_tags(tag, limit)`, `notes(topic, generation, tags, tags_all, limit)`, plus the pure `label_tags` rule. `topic.rs` was 783 lines and these are about 350 more.
- One read transaction per reader, and the FIRST statement is the active-pin read (`read_status_and_meta`), because a deferred SQLite read transaction takes its snapshot at its first read. State, counts, labels and exemplars therefore come from one generation. The pin is read inside the transaction (not through `active_embedding_model()`, which would be a separate statement); a missing pin reads as model `""`, so an unpinned index is `Absent` and an unpinned index holding a generation is `Stale`, never an error.
- Test seam: each public reader calls a private `*_with` twin taking a `between: &dyn Fn()` hook, run after the state read and before any count. The two-connection tests commit `replace_generation` from a second `SearchIndex::open` on the same WAL file inside that hook. No test-only API leaks out of `vault`.
- `notes` returns an enum, `TopicNotes { Unavailable, GenerationMismatch{requested, stored}, UnknownTopic{topic, k}, Notes }`, so the generation check and the topic-range check happen in the same transaction as the read and oracle cannot forget either. Oracle maps the latter two to `CallToolResult::error` (`is_error: true`), Unavailable to the `absent`/`stale` envelope.
- Label rule: threshold test and sort key use integer-exact arithmetic (`lift = count*assigned / (overall*topic_notes)`, one division of exact integers), so a lift of exactly 2.0 labels and equal lifts tie-break by tag name, not by rounding. The count floor is `max(5, topic_notes*20/100)` (integer, so rounded down). Exemplars come from one `ROW_NUMBER() OVER (PARTITION BY topic ORDER BY cosine DESC, note_path)` statement.
- Labels are always computed from the full topic (sizes, per-topic tag counts and overall tag counts are unfiltered); only the `notes` count honors `tags`. `in_tags` is `overview` with one tag, a limit and no exemplars (the `tag_brief` shape is `{topic, notes, tags}`).
- `topic_map`: `limit` applies to the one-topic note list only (default 20). The topic list is never truncated, because the criterion is that browse returns every non-empty topic. Envelope `{status, generation, trained-at, k, count, results}` plus `topic` in the note form and `hint` when absent or stale. `trained-at` is RFC 3339 (chrono is already an oracle dependency), not raw unix seconds, because an agent reads it.
- `tag_brief` `topics`: `{generation, results}`, `null` plus `topics-status` when not Valid. `vault_overview`: `serde_json::to_value(&stats)` plus one `topics` key built from `SearchIndex::topic_snapshot`; `VaultStats` and `sb oracle stats` are untouched.
- A corrupt topic state is an error from every reader (`Self::err`), including `tag_brief` and `vault_overview`: a broken topic table fails those two tools loudly rather than quietly dropping the key. That follows the doc's "corrupt state is an error at every caller".
- `TopicStatus` (already `Serialize`, lowercase) is serialized directly into the envelope. Docs: `oracle/AGENTS.md`, `vault/src/search/AGENTS.md`, and a "Topic map" section in `CLAUDE.md`.
- Test-util: `SearchIndex::set_test_tags` (replaces a note's `note_tags` rows) in `vault/src/search/topic.rs`, for fixtures that need the facet without an indexed file.
- `topic_map` wall-clock, local fixture only (3,333 notes, 116 tags with 4 per note, k=64, in-memory index, 7 calls each, ms): no arguments min 11.6 / median 11.7 / max 12.1 (release profile) and 15.0 / 15.1 / 15.5 (debug); `tags: ["tag3"]` median 12.3 release, 15.5 debug; `topic: 5` median 0.2 release, 0.4 debug. This is an in-memory fixture with no concurrent writer, not ripr.
### Deviations
- The doc says "Phase 5 records `topic_map` wall-clock on ripr". Not done: I never touch ripr. The local fixture numbers above stand in; the ripr timing is an operator step after `otto deploy` and `sb cortex topics --train` (`time sb oracle call topic_map`).
- One reader beyond the doc's three: `SearchIndex::topic_snapshot()`, for the `vault_overview` block. Computing the full overview for a four-field block would run every label query.
- Fixture tests build generations with `replace_generation` directly (planted assignments), not through cortex `train`, since the readers' contract is the three tables and oracle does not depend on cortex.
- The oracle tests are `oracle/src/server/topic.rs` (declared `#[cfg(test)] mod topic;`), not a `tests.rs`: `server/tests.rs` is at the size limit, and `topic/tests.rs` would need a non-test `topic.rs` parent. The only edit to `server/tests.rs` is adding `topic_map` to `NOTE_TOOLS`, which the coverage guard in `note_returning_tools_advertise_trace_block` requires.
### Tradeoffs
- One transaction, five-plus statements, labels computed per call vs a cached label table: chose compute-per-call, since tags change without a retrain and a stored label is what the doc rules out. Cost is the 12 ms above on a 3,333-note fixture.
- `notes` fetches full `NoteRow`s through `get_note` after the transaction closes vs joining note columns into the reader: kept `get_note` and `format_note`, so every note-shaped surface formats identically. The topic membership (which paths, in what order) is still from the one snapshot; a note deleted in between is skipped with the existing WARN.
- Unknown topic id is an error vs an empty list: error (fail loudly); an id outside `0..k` is the caller's mistake, and an empty list would read as "empty topic".
### Open questions
- Scott's acceptance step: read `sb oracle call topic_map` and `sb oracle call topic_map --json '{"tags":["llm"]}'` on the live index and rule the output usable.
- ripr wall-clock for `topic_map` (and the SQLite busy-timeout risk row) is still unmeasured.

Bite check for the two-connection tests: in `overview_with` and `notes_with`, right after `between()` I committed the transaction and opened a fresh one (the reader loses its single snapshot). `a_generation_committed_mid_read_does_not_leak_into_the_response` failed on `assigned count is generation 1's` (left 2, right 6) and `a_generation_committed_mid_notes_read_does_not_leak_into_the_response` failed on `topic 1 of generation 1, not generation 2's topic 1` (left 0, right 3). Restored; 18/18 `topic::read` tests pass.

## Phase 6: Embedding revision marker
### Design decisions
- One hash function, `vault::search::vector_hash(blob) -> String` (`vault/src/search/revision.rs`): lowercase hex SHA-256 of the exact stored bytes. The three production writers call it (`insert_batch_item` under `upsert_embeddings_batch`/`swap_embeddings_batch`, `upsert_embedding`, `swap_kind_chunks`), each hashing the same `encode_embedding_bytes` output it stores.
- `sha2 = "0.11.0"` moved to `[workspace.dependencies]`; cortex and vault use `workspace = true`. Vault's is optional, pulled by the `search` feature, because the migration is not vec-gated (`edge_build_state` exists in every build). borg keeps its own `sha2 0.10`, untouched.
- `note_embeddings.vector_hash TEXT` is nullable in the fresh `CREATE TABLE` as well as on migrated DBs: `ALTER ADD COLUMN` cannot add a NOT NULL without a default, and fresh and migrated tables should have one shape.
- `SearchIndex::migrate_vector_hash` (`revision.rs`), called from `ensure_schema` after the vec block (so the active pin and model-id rename are settled first): one `BEGIN IMMEDIATE` transaction that adds the column, hashes every NULL row in chunks of 2,048, and swaps `edge_build_state` (add `semantic_vector_hash`, backfill fresh-under-old-rule notes with their active-model summary hash, `DROP COLUMN semantic_built_at`). Idempotent: a second run returns an all-zero report and touches nothing.
- A partial index `idx_note_embeddings_unhashed ON note_embeddings(id) WHERE vector_hash IS NULL` makes the every-open "any NULL?" check read an empty index rather than every blob. The backfill runs on every open, not only when the column is added, so a row an older binary writes after the migration is healed on the next open (tested).
- Readers error on a NULL hash (`revision::unhashed_row`): `semantic_edge_targets`, `note_summary_vector_hash`, and `summary_vectors` (`unassigned`, `in_scope_summaries`). Their SQL selects NULL-hash rows on purpose so the error fires instead of SQL's NULL comparison reading as "unchanged".
- Topics: `note_topics.vector_hash TEXT NOT NULL` replaces `embedding_id` in the `CREATE TABLE` (unshipped). `Assignment.vector_hash` and `SummaryVector.vector_hash` replace `embedding_id`; `TopicTables` carries the hash as a `String`. The predicate is `t.note_path IS NULL OR e.vector_hash IS NULL OR t.vector_hash != e.vector_hash`, in SQL.
- Graph: `edge_build_state.semantic_vector_hash TEXT` replaces `semantic_built_at`; `record_edge_build(path, content_built_at, Option<&str>)`; `note_summary_produced_at` renamed `note_summary_vector_hash` and returns `Option<String>` (None = no active-model summary, recorded as NULL). `cortex::graph::record_watermark` is the one caller.
- Bundled SQLite is 3.49.1 (`libsqlite3-sys 0.32.0`), so `ALTER TABLE DROP COLUMN` (3.35+) is available; the migration test exercises it.
- Docs: `vault/src/search/AGENTS.md` (the `vector_hash` column, the `edge_build_state` contract, the `unassigned` predicate, `note_topics.vector_hash`), `cortex/AGENTS.md` (graph freshness wording), `CLAUDE.md` (one "Re-embed marker" bullet in the Topic map section; CLAUDE.md did not name `produced_at` as the trigger or `embedding_id` anywhere, so nothing there needed correcting).
- Tests added: `revision::tests::vector_hash_is_lowercase_hex_sha256_of_the_exact_bytes`; `revision::tests::vec::{every_note_embeddings_writer_stores_the_hash_of_its_blob, migration_hashes_every_row_carries_fresh_notes_over_and_drops_semantic_built_at, a_row_an_older_binary_writes_after_the_migration_is_hashed_on_the_next_open, a_null_vector_hash_reached_by_a_reader_is_an_error, a_second_re_embed_inside_one_second_makes_the_note_a_semantic_target_again, a_note_built_while_unembedded_becomes_a_target_when_its_summary_lands}`; `cortex::topics::tests::assign_reassigns_the_re_embedded_note_holding_the_max_embedding_id`. Tests changed (updated, none deleted): `vault::search::topic::tests` (helper `embedding_id` -> `stored_hash`, the dump tuple, `unassigned_lists_in_scope_notes_without_a_current_topic`'s hash assertion and comment, `peek_topics_reports_counts_and_the_since_train_cosine`), the planted assignments in `topic/read/tests.rs`, `oracle/src/server/topic.rs` and `sb/src/cli/checks/topics/tests.rs` (`vector_hash: "planted-..."`), and the comments in `cortex::graph::tests::semantic_edge_not_stranded_when_embedding_lands_after_skip`, which said it was keyed on `produced_at`; its logic holds unchanged under the hash rule. `cortex::graph::tests::record_watermark_propagates_a_read_failure` passes unchanged (the active-pin read still fails first).
- Test accessor `SearchIndex::test_embedding_ids()` (test-util) so the cortex test can prove the re-embed reused the max id.
### Deviations
- The doc says "`note_summary_produced_at` becomes a hash reader"; it is renamed `note_summary_vector_hash` (the task asked for a truthful name) and returns `Option<String>` rather than a 0 sentinel.
- The migration runs as one function covering both tables, not "beside the `recipe` column migration" inside `ensure_vec_schema`: the `edge_build_state` swap must run in every build and after the pin is settled, so it sits at the end of `ensure_schema`. Same effect, correct seam.
- The raw-SQL `INSERT INTO note_embeddings` statements in `vault/src/search/tests/group_b.rs` (CHECK, UNIQUE, CASCADE and stale-removal tests) and `schema/tests.rs` (old-schema fixtures) were left without `vector_hash`. They test constraints and old shapes, not the writer, reach no marker reader, and a NULL they leave is exactly what the next-open backfill heals.
### Tradeoffs
- A partial index on unhashed rows vs a full scan for NULLs on every open: the scan reads past every blob (the column sits after `embedding`), the empty partial index costs nothing in steady state.
- Hashing in chunks inside one transaction vs one SELECT of all blobs: the chunked form bounds memory (2,048 x 1.5 KB) with the same atomicity.
- Timing, local fixture (35,000 `note_embeddings` rows of d=384, 5,000 notes, 2,500 of them fresh under the old rule, 78 MB file on local disk, warm page cache): the migrating `SearchIndex::open` took 276 ms (debug test profile, deps at opt-level 2) and 214 ms (release); the next open took 2.2 ms. The harness was a throwaway `#[ignore]` test, not committed.
- Bite checks: (1) the new cortex max-rowid test, ported to Phase 5 code (`8f5061e`, a worktree, the id assertions dropped because the accessor does not exist there), fails `assigned_this_run` left 0 right 1. (2) The same-second graph test ported to the `produced_at` rule at `8f5061e` fails: `semantic_edge_targets` left `[]` right `["a.md"]`. (3) Mutating the fresh-note backfill to `WHERE 0 AND ...` fails the migration test (`fresh.md` becomes a target). (4) Mutating `summary_vectors` to default a NULL hash to `""` fails the NULL-reader test. All restored.
### Open questions
- Rollback: after this migration an older binary (v0.21.1 or the Phase 1-5 builds) can still read and embed (its NULL hashes are healed on the next new-binary open), but its graph pass fails, because `record_edge_build` and `semantic_edge_targets` name the dropped `semantic_built_at`. The doc rules the column dropped; the operator should know a rollback past this commit breaks `sb cortex graph` until roll-forward.
- A local index that already holds a Phase 3-5 `note_topics` table (with `embedding_id`) keeps that shape, since the doc changed the `CREATE TABLE` in place with no migration; `assign_batch` against it fails loudly on the missing column. Only matters on a dev machine that trained before this commit: `sb cortex topics --clear` does not fix it, dropping the three topic tables (or a fresh index) does. ripr runs the installed v0.21.1, which predates Phase 3.

## Phase 7: Audit cheap-wins
### Design decisions
- One helper, `vault::search::unix_to_rfc3339(secs) -> Result<String>` (`vault/src/search/topic.rs`, re-exported from `vault::search`): seconds precision, `Z` spelling, error on an unrepresentable time. Cortex's private `rfc3339` and oracle's `unix_to_rfc3339` (`Option`, `+00:00`) are deleted; both call the vault one.
- Oracle's `topic_envelope`, `topic_unavailable` and `topics_status_block` return `eyre::Result`, so an unrepresentable `trained-at` is a tool error on `topic_map` and `vault_overview`, not `null` (`oracle/src/server.rs`).
- `topic_map` browse and `tags` forms check `generation` through `generation_mismatch` against the Valid snapshot, sharing `generation_mismatch_error` with the topic form (one message, both numbers). Absent/Stale still answers with its status envelope: no stored generation to compare.
- `TopicState::meta` parses a surviving `generation` in its Absent branch; `peek_topics` errors on 1 or 2 of the 3 topic tables and reads 0 as Absent (`vault/src/search/topic.rs`).
- `assign_index_until` re-reads `stopped()` after building the batch, before `assign_batch`; a stop there sets `was_stopped` and breaks (`cortex/src/topics.rs`).
- Out-of-range topic error reads `topics 0 to {k-1}`.
### Deviations
- The helper's own test (`unix_to_rfc3339_spells_z_at_seconds_and_errors_out_of_range`) cannot fail before the fix: the function did not exist. The bite for item 5 is carried by the two oracle tests that go through the surfaces.
- Two older oracle tests (`topic_map_browse_returns_every_non_empty_topic_with_the_envelope`, `vault_overview_carries_a_topics_status_block`) pinned the `+00:00` spelling; inverted to `Z`.
- The unknown-topic test's pinned text changed from `0..2` to `0 to 1` (item 6).
### Tradeoffs
- Fail the whole `topic_map` / `vault_overview` call on an unrepresentable `trained-at` vs. dropping the field: fail-loudly rule, and the state is corrupt anyway.
### Open questions
- None.

Bite evidence (each test run against the unfixed code before its fix):
- Item 2: `topic_map_browse_and_tags_forms_check_generation_too` FAILED (browse and tags returned success).
- Item 3: `assign_stopped_between_the_read_and_the_write_writes_nothing` FAILED (cortex).
- Item 5: `trained_at_is_rfc3339_with_a_z_and_seconds_on_every_surface` and `an_unrepresentable_trained_at_is_an_error_not_a_null` FAILED (oracle).
- Item 6: `topic_map_unknown_topic_and_unknown_arguments_fail_loudly` FAILED on the new `0 to 1` assertion.
- Item 7: `peek_topics_errors_when_only_some_topic_tables_exist` and `a_surviving_unparseable_generation_with_no_trained_keys_is_an_error` FAILED (vault, `--features vec-candle`).
