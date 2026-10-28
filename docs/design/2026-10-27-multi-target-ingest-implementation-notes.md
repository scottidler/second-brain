# Implementation Notes: Multi-target `sb borg ingest` with one bulk summary

Design doc: `docs/design/2026-10-27-multi-target-ingest.md`

Entry shape: decision | file:function | why.

## Phase 0: Guard counts every `borg ingest` target

Landed in `scottidler/claude` as commit 02486b2 (not in this repo).

### Design decisions
- Target counting moved into one helper | `HOME/.claude/hooks/intent-guard.sh:ingest_count` | it reads the UNMASKED statement through lib.sh's `args`, so a quoted operand counts as a target and a quoted `--note` value stays one token. The verb gate still runs on the quote-masked copy, so `echo "sb borg ingest ..."` prose stays out of scope.
- Redirects are stripped before tokenising, with the same sed the LN and push-refspec walks use | `ingest_count` | otherwise `args` hands back both halves of `2>&1` as bare operands.
- `-t`/`--tags` values are consumed greedily (clap's `num_args = 0..`) only while tag-shaped (`[A-Za-z0-9_-]+`); a URL or path after `-t` counts as a target | `ingest_count` | fail closed: clap would swallow it as a tag, but it was meant as a target, and Phase 1 exits 2 on exactly that shape.
- `--tags=x` / `-tx` attached forms end tag mode | `ingest_count` | the conservative reading; anything after them counts.
- An unknowable operand denies before the door is read: glob (`* ? [`), brace expansion (`{a,b}`, `{1..3}`), any `$` or backtick, and stdin (`-`, `--from`, `--from=`, `--stdin`) | per-statement INGEST block | no `<n>` can be compared to an unknown count. A URL is exempt from the glob test because `?` spells a query string.
- No operand at all (`--clipboard`) still counts as one target | per-statement INGEST block | keeps the old floor of one op per verb occurrence.
- `reingest` / `reingest-failed` keep the occurrence count | per-statement INGEST block | they take no target operand.
- The door deny texts say "ingest targets" instead of "ingest occurrences" | `INGEST_DOOR_DENY` and the ceiling deny | names tell the truth about what is compared.

### Deviations
- The doc names `*"ingest --file"*` as the one clause to retire. The first-operand literal-URL clause (`:1952` in the doc) is retired too: its job (refusing a non-literal operand) is now done per operand by `ingest_count`, and it would have denied the lone-path case the doc requires to allow.
- The command-scope `url_targets` recount over the scan was removed: per-statement counting already counts targets.
- Heredoc bodies keep the old occurrence count (`heredoc_ops`). A per-line target count was written first, then measured to change no verdict: any `.sh` heredoc carrying an ingest already denies on the command-scope `<` redirect clause (the `<<` itself matches it). Reverted to keep the change minimal.
- Fixtures pinning the retired `--file` deny were changed, not deleted. `sb borg ingest --file urls.txt` is inverted to allow with a dated comment. `sb borg log; ... --file urls.txt`, the `.sh` heredoc `--file` case, and the M6 door-laundering case now use `./a.pdf ./b.pdf`, so they still test what they were written for (read-only verbs not short-circuiting, heredoc scanning, per-statement door).

### Tradeoffs
- Greedy-but-tag-shaped `-t` vs strictly greedy `-t` (clap's literal behavior): strictly greedy would let `-t ai https://a https://b` count zero targets and hide a bulk behind a tag flag.
- Deny any glob character in a non-URL operand, quoted or not, vs only unquoted globs: a quoted `'./*.pdf'` reaches the CLI literally and fails classification anyway; one rule is simpler and fails closed.
- Plain bash walker over `args` output vs a new mode in lib.sh's awk: the walk is INGEST-local, and redirect-strip-then-`args` is the in-file precedent (LN, push refspecs, DELETE-REF).

### Open questions
- The uncommitted `--help`/`-h` carve-out in `intent-guard.sh` has a comment saying the old failure "died in the literal-URL clause on the `2>&1` token". That clause no longer exists after Phase 0; reword the comment when the carve-out is committed. Its five test cases pass on top of Phase 0 (671 pass, 0 fail).
- A `.sh` heredoc that writes even one literal ingest target denies (the `<` redirect clause), as it did before Phase 0. Left as is; confirm that is the intended scope.

## Phase 1: Multi-target CLI and classification
### Design decisions
- Orchestration lives in the library, not in `sb`: `borg::ingest_targets` (`borg/src/client/targets.rs`) | `ingest_targets`, `classify_targets` | libraries return typed data (`IngestRun`, `TargetReport`), `sb` prints and picks the exit code, and the toast seam and file-start ordering become testable against a stub daemon.
- The toast is an injected `impl Fn(&str, &str)`; the file ingester is an injected closure; `on_report` fires per finished target | `ingest_targets` | a test can count toasts and observe how many POSTs had landed when the first file starts; `sb` prints each line live.
- `ingest` is now a thin wrapper over `post_ingest(config, ..., toast)`, which takes the toast as `&dyn Fn` | `borg/src/client.rs` | one POST path for single-target/hotkey/clipboard (real toast) and multi-target (silent per target).
- A single target keeps the per-failure toast and sends no aggregate; a multi-target run passes a silent per-target toast and sends one toast with summary `<k> of <n> submits failed` (rendered `borg: <k> of <n> submits failed`) when k > 0 | `ingest_targets`.
- `IngestOutcome::Queued { trace_id: String }`; a daemon `Queued` answer with no trace id is an error, not an empty id | `post_ingest`. The in-process `note`/`ingest_file` paths put the trace id they generated in `Queued`, so the field is never a lie.
- Classification is stricter than "existing path": a directory is a usage error (exit 2) because the local pipeline would only fail on it later | `classify_targets`.
- A usage error is a typed `borg::UsageError`; `sb` prints it and returns `ExitWith(2)` before any submit. `--clipboard` and "no targets and no clipboard" are clap-level (`conflicts_with`, `required_unless_present`), so they exit 2 through clap.
- Exit: 0 all good, 1 any URL submit or file ingest failed (including `IngestOutcome::Failed`), 2 usage. Failed lines go to stderr, the rest to stdout.
- Test stub: `stub::Behavior::IngestDropping` (borg lib tests) and a counting thread stub in `sb/tests/ingest.rs` (subprocess tests, `BORG_DISABLE_NOTIFICATIONS=1`), the latter modelled on `sb/tests/wait.rs`.

### Deviations
- A `Duplicate` outcome renders as `<target> -> ingested (duplicate of <date>)` and exits 0 | the doc lists three line forms and no duplicate form; this keeps today's exit-0 behavior for a duplicate.
- The single-clipboard path keeps `print_ingest_outcome` (now `Queued for processing: <trace>`) rather than the per-target line | it has no target string and the doc scopes per-target lines to the targets form.
- "exactly 1 client toast call" for one failed POST of three is proved in a borg lib test with a counting toast closure; the `sb` subprocess test proves exit 1 and that every URL was attempted. A subprocess cannot count toasts.

### Tradeoffs
- Orchestrator in `borg` vs in `sb/src/cli/borg.rs`: borg, so the injected seams are unit-testable; the cost is a wider `borg` public API (`ingest_targets`, `classify_targets`, `Target`, `TargetReport`, `IngestRun`, `UsageError`, `send_notification`).
- Keep going after a failed submit vs abort the run: keep going (each target is independent, and the doc says file targets still run after failures). With the daemon down this costs one connect failure per URL, with no toast per URL.

### Open questions
- None.

## Phase 2: ingest.bulk-threshold config
### Design decisions
- `IngestConfig { bulk_threshold: u32 }` (kebab-case, `Default` = 5) under `ingest:` with `#[serde(default)]` on `Config` - borg/src/config.rs - an absent block behaves as 5.
- `is_bulk(url_count, total_targets, threshold, clipboard)` is a pure function - borg/src/client/targets.rs - the whole rule in one testable place; threshold 0 falls out of `total > 0` with at least one URL.
- `IngestRun.bulk` carries the decision, computed in `ingest_targets` from the URL/total counts and `config.ingest.bulk_threshold`. Nothing reads it yet; Phase 3 wires the batch.
### Deviations
- `ingest_targets` passes `clipboard = false` to `is_bulk`: `--clipboard` returns in `sb` before reaching the orchestrator, so the flag never arrives there. The `clipboard` parameter exists so the rule is stated and tested in one place.
### Tradeoffs
- A `bool` on `IngestRun` vs a separate call from `sb` - keeps the decision beside the counts it needs, and Phase 3 already needs it there.
### Open questions
- None.

## Phase 3: Batch identity, seal, and the one summary
### Design decisions
- One new module owns the batch: `borg/src/batch.rs` (`create_if_absent`, `seal`, `drained`, `maybe_summarize`, `after_terminal`, `backstop`, `BatchSummary::text`) plus `borg/src/batch/notifier.rs` (`run`, `run_with`, `deliver`) | the drain check, claim, and summary text live in one place; nothing in `batch.rs` touches a sink.
- The drain check after a pipeline terminal write runs in `/ingest`'s detached task, right after `dispatch_ingest` returns (`routes::summarize_if_drained`, on the blocking pool) | `process_content` writes the terminal row before it returns on every path, only `/ingest` can carry a batch, and the task already holds `AppState`, so the summary sender is threaded, never a process global.
- The watchdog pass returns data: `watchdog::run_once_conn -> Pass { promoted, summaries }`; `watchdog::run(config, sender)` puts the summaries on the channel | a crash promotion runs `batch::after_terminal`, then the pass runs `batch::backstop` on the same `hard_timeout_secs + 60` deadline; a batch error is logged and never stops a promotion.
- The notifier reuses `notify::MessageSink` + `announce_unless`; `Desktop` gained a `MessageSink` impl (one fresh popup, `notify::Desktop::popup`) | the summary is one plain-text message per sink, the shape `MessageSink` already names, and the disable gate stays in one place. Outcome mapping: sink absent or gate closed -> `disabled`, `Announced::Failed` -> `failed` (with a WARN carrying the reason).
- `count` runs before the claim UPDATE (`batch::claim`) | a failed count leaves the claim for the next trigger instead of claiming and losing the summary.
- The backstop claim adds `AND sealed_at IS NULL` to the claim UPDATE as well as to its SELECT | a seal landing between the two is never summarized as "seal missing".
- The door refuses a batch id that is not 1-64 ASCII letters, digits or `-` before intake (`batch::valid_id`, `routes::ingest`), like a bad `replay-origin` | the id is client input that becomes a primary key and a URL path segment.
- `seal` compares id sets (sorted, deduplicated) and stores them sorted | a client retry that reorders ids is an identical re-seal (200), not a 409.
- Client half in `borg/src/client/seal.rs` (`require_bulk_feature`, `seal_batch`); `ingest_targets` now returns `Result<IngestRun>`, `Err` only for the `bulk-batch` refusal before any submit; `IngestRun.batch: Option<BatchReport { id, seal: SealStatus }>`; `any_failed` includes a failed seal | `sb` prints the batch line (stderr on seal failure) and exits 1 through the existing `any_failed`.
- Receipts v9: `batch_id` in the baseline `schema.sql` plus a `has_column` ALTER; the index is created in `run_migrations` after the ALTER; `batches` is a `CREATE TABLE IF NOT EXISTS` in `schema.sql` (the `remediations` precedent) | on an old DB the baseline batch runs before the column exists, so an index in `schema.sql` would fail.
- `receipts::record_received_in_batch` is a separate entry point; intake creates the `batches` row first, then the receipts row with `batch_id` in the same INSERT (`intake::record_received_with_sidecar_to`) | a member never exists without its batch row.

### Deviations
- `maybe_summarize` returns `Result<Option<BatchSummary>>`, not `Option<BatchSummary>` | a DB error is loud at the caller (logged) instead of reading as "not drained". Same effect, correct seam.
- The doc lists `mark_succeeded`, `mark_failed`, `mark_rejected` as the call sites. The check runs after `process_content` returns in `/ingest`'s task (which covers both terminal writes) and after `promote_single_to_crashed` in the watchdog. `mark_rejected` is not hooked: it is harvest-only, runs in a separate process with no notifier, and a harvest row never carries a batch. `record_failure_at_door` is not hooked either: `/ingest` refuses before intake, so no batched row ever reaches it.
- `uuid` was not a direct `borg` dependency (the doc says it was); added with `cargo add uuid --features v4 -p borg` (it was already in the lock transitively).
- `borg/AGENTS.md` got one module-map entry for `batch.rs` | `otto ci`'s `agents-map` lint fails on an undocumented module. The invariant/notification prose stays Phase 4.
- "0 per-item sends" is proved at the per-item gate: the tests route a per-item ack to the counting sinks exactly when `dispatch::pings_operator(method, None)` is true, which is the gate `/ingest` and `dispatch_ingest` apply. The per-item path itself takes the concrete `Desktop`/`Telegram` types, so it cannot hold a counting fake without a refactor this phase does not call for.
- A bulk with zero `Queued` URL items seals nothing and renders `batch <id>: no URL item was queued; nothing to seal` | the doc gives the sealed and seal-failed lines only. Rows whose response was lost still reach the seal-missing backstop.
- `drained` checks every sealed id has a receipts row (any batch); `unknown` counts sealed ids with no row carrying this batch id | the doc's two halves read literally; with a well-behaved client `unknown` is 0.
- `rejected` member rows count into `failed` in the summary | harvest-only, never expected in a batch; keeps succeeded + failed = total.

### Tradeoffs
- Drain check in the `/ingest` task vs inside `pipeline::record_terminal_to_receipts` behind a global sender: the task already has the sender and the batch id, so no process global and no extra read for every unbatched trace in every transport.
- `Pass` return value vs passing the sender into `run_once_conn`: the pass stays pure data, and the existing watchdog tests only needed `.promoted`.
- An unbounded `mpsc` vs bounded: at most one summary per batch ever enters it, so a bound buys nothing and a full channel would add a failure mode to a receipts write path.

### Open questions
- A sealed id whose row never appears (a client bug, not a lost response) holds its batch forever: the backstop is seal-missing only, per the doc. Confirm that is acceptable, or whether a sealed batch should also get an age backstop once no member is `received`.

## Phase 4: Docs, ship, live check (docs portion)
### Design decisions
- Docs describe shipped reality from Phases 0-3, not the plan | `borg/AGENTS.md`, `sb/AGENTS.md`, `CLAUDE.md` | e.g. the drain check is documented at its real call sites (`/ingest` task, seal, watchdog promotion), not at `mark_succeeded`/`mark_failed`.
- Durable docs name files and symbols only, no line numbers, counts, or version pins | all three docs | house rule in `general.md`.
- `borg/AGENTS.md` invariant 3 carries the batch-summary mechanism and invariant 5 carries the `batch` field; a new multi-target orchestrator bullet sits beside the CLI-helpers bullet | `borg/AGENTS.md` | the notifications and additive-field invariants are where a reader looks for them.
- The `batch.rs` module-map line Phase 3 added was left as is | `borg/AGENTS.md` | extend, not duplicate.
- `config/templates/borg.yml.example` `ingest:` block checked and left unchanged | it already documents `bulk-threshold: 5`, 0 semantics, and `--clipboard` never being a bulk.
- Design doc Status flipped to Implemented in this commit | `docs/design/2026-10-27-multi-target-ingest.md`.
### Deviations
- Bump, `otto deploy` on ripr, the live 6-URL bulk, and the `dbus-monitor`/Telegram success criteria are NOT done here | they run at the user-approved finalization checkpoint. The doc's Phase 4 live criteria are therefore unverified at this commit, yet Status reads Implemented per the final-phase duty.
### Tradeoffs
- Long single-line bullets in `borg/AGENTS.md` vs a new sub-node | the file's existing style is one dense bullet per fact; a new `AGENTS.md` node would trip the agents-map lint for no gain.
### Open questions
- The Phase 3 question stands: a sealed id whose row never appears holds its batch forever (backstop is seal-missing only). Confirm acceptable.
- Phase 0 leftovers in `scottidler/claude`: reword the `--help` carve-out comment in `intent-guard.sh` when it is committed.

## Audit fix-forward: guard
Implementation audit must-fix items 1 and 2 against Phase 0, committed in `scottidler/claude` as 072035e on top of 02486b2.
### Design decisions
- Redirects are stripped by a quote-aware awk, `strip_redirects` in `HOME/.claude/hooks/intent-guard.sh`, before `args` tokenises | the old `sed` took the `>` in `--note "a > b"` for a redirect, ate the closing quote, and every later target merged into one unterminated word. Probe `sb borg ingest --note "a > b" https://a https://b https://c`: deny before 02486b2, allow at 02486b2, deny now.
- One shared `BORG_VERB_RE` (`borg`, then any `-flag [value]` pairs, then the verb) feeds the statement gate, the `reingest` op count, the `ingest` gate, and the heredoc op count | all four required `borg` adjacent to the verb, so `sb borg -c X ingest a b` and `sb borg -l warn reingest --all` skipped INGEST entirely.
- `ingest_count` walks past borg's options generically: a bare word after a flag is its value, the first bare word that is neither is another subcommand and stops the walk | robust to options added later to `BorgCli` or sb's global `Cli`, without a table to keep in sync.
- After the verb, `-l`/`--log-level` consumes its value | sb's `--log-level` is `global = true`, so `sb borg ingest -l warn https://a` is valid and `warn` is not a target. 02486b2 counted it and denied a one-target ingest.
- The brace test now runs before the `http(s)://` exemption, which keeps only `*`, `?`, `[` | `https://x/{a,b}` counted 1 and the shell expands it to 2. A brace operand is unknowable, door or not, and says "brace expansion operand" in the deny reason.
### Deviations
- A QUOTED brace URL (`"https://x/{a,b}"`) also denies, though the shell would not expand it | `args` drops the quote characters, so `ingest_count` cannot tell the two apart. Fail closed, same as a quoted glob path already was.
- `reingest` takes no target operand, so only the borg-options fix applies to it; no brace or redirect work was needed there.
### Tradeoffs
- A local awk in `intent-guard.sh` vs a new `lib.sh` mode | the strip is used by one caller; putting it in `lib.sh` widens the blast radius to every hook and its own test suite for no second consumer.
- Generic option skipping vs an explicit `-c/--config/-l/--log-level` table | a table goes stale silently the day a borg option is added, which is the exact way this rule failed.
### Open questions
- `sb borg reingest --jobs 4 --all` still escapes the `--all` clause, which matches the literal `ingest --all` substring. Pre-existing and outside items 1 and 2, so left alone. Should `--all` match anywhere in an ingest or reingest statement?

## Audit fix-forward: batch
### Design decisions
- Sealed-batch reconciliation on every watchdog pass (`borg/src/batch.rs:reconcile_sealed`, called from `borg/src/watchdog.rs:run_once_conn` after crash promotion and before the backstop). A batch whose last terminal write committed but whose `/ingest` drain check never ran (restart, busy DB) was stranded: the backstop skips sealed batches and promotion only re-checks the traces it promotes. Reconciliation runs the ordinary drain predicate, so a sealed id with no receipts row still holds the batch and there is no age override.
- The claim UPDATE restates the drain predicate (`borg/src/batch.rs:CLAIM_DRAINED_SQL` / `CLAIM_SEAL_MISSING_SQL`, used by `claim`). A member whose intake lands after `drained` said yes now fails the claim instead of being summarized past.
- Count and claim run in one `BEGIN IMMEDIATE` transaction (`borg/src/batch.rs:claim`), so the reported counts are the membership the claim was judged on.
- `--note` conflicts with file targets by presence, not content (`borg/src/client/targets.rs:classify_targets`).
- Per-item ping proof goes through production code: `borg/src/notify/probe.rs` is a test-only record written by `Telegram::processing/result` and `Desktop::processing/result`; route and dispatch tests use live sinks in `AppState`, so deleting either gate fails a test.
### Deviations
- None from the design. The batch tests that re-enacted the `pings_operator` gate were narrowed to summary delivery and renamed; the per-item half moved to route and dispatch tests.
### Tradeoffs
- Guarded UPDATE plus `BEGIN IMMEDIATE` vs a transaction around the whole drain check: the guard alone makes the claim correct; the transaction only keeps counts consistent and stays short.
- A test-only probe in the concrete sinks vs a sink trait for per-item pings: the probe adds four `cfg(test)` lines; a trait would reshape the trait-free per-item sink design for a test.
- Reconciliation re-checks held sealed batches every pass, indefinitely: a few indexed reads each, and the audit ruled holding such a batch acceptable.
### Open questions
- None.
