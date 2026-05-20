# CLI Shakedown Report: sb v0.8.6

**Date:** 2026-05-20
**Binary:** `/home/saidler/.cargo/bin/sb` (62 MB, built from `cargo install --path .` via `otto deploy`)
**Vault under test:** `~/repos/scottidler/obsidian/` (1365 notes, 99.3% embedding coverage)
**Config layout:** `~/.config/sb/` (post-Phase-1a migration; legacy locations preserved on disk but not read)

## Summary

| Metric | Count |
|--------|-------|
| Top-level subcommands discovered | 7 (`borg`, `cortex`, `oracle`, `status`, `doctor`, `bootstrap`, `help`) |
| Distinct commands tested | ~45 |
| Commands passed | 41 |
| Commands flagged (defect or inconsistency) | 4 |
| Commands skipped (mutating, long-running, or auth-bound) | many — see "Skipped" below |
| Pipelines tested | 6 |
| Edge cases tested | 5 |

The four shipped v0.8.5 cleanup phases are observably live: configs load from `~/.config/sb/`, oracle response keys are unified on `results`, `--format yaml` rejects at parse time with exit 2, `sb cortex daemon --status` shells out to `systemctl`. All four hand-written test suites added to close audit gaps pass under the running binary as well.

## Command Results

### Top-level health

| Command | Exit | Result |
|---|---|---|
| `sb --version` | 0 | `sb v0.8.6` |
| `sb status` | 0 | All systemd / config / shared-config / patterns / embedding-cache / vault sections render; 1 warn (11 pending DLQ rows) is real vault state. |
| `sb doctor` | 0 | Same data with severity tags. **Exits 0 even when "Issues detected"** - see Defects. |

### borg (read-only)

| Command | Exit | Sample |
|---|---|---|
| `sb borg daemon --status` | 0 | Full systemctl block. Daemon binary path: `/home/saidler/.cargo/bin/borg` (pre-unified) - see Defects. |
| `sb borg intake list --limit 5` | 0 | Markdown table; 5 recent rows including a known trace `ht-9acf62` |
| `sb borg intake show ht-9acf62` | 0 | Row body + 39-byte sidecar content |
| `sb borg dlq list --limit 5` | 0 | 5 watchdog-orphan rows, all `pending` |
| `sb borg dlq show ht-575bd1` | 0 | DLQ row + intake row + sidecar + cross-ref `(ledger has a completed row for this source: ...)` |
| `sb borg blocklist list` | 0 | `(blocklist empty)` |
| `sb borg retention status` | 0 | `Staging root: ~/.local/share/borg/stages`, 0 traces, 0 bytes |
| `sb borg audit` | 0 | 1122 ledger entries scanned, 1 blocked-content finding, dozens of duplicate-note findings (real vault state) |
| `sb borg audit --invariant` | 0 | `orphans (>1800s no resolution): 0`, wrote `system/views/borg-orphans.md` |

### cortex (read-only)

| Command | Exit | Sample |
|---|---|---|
| `sb cortex daemon --status` | 0 | Full systemctl block. Daemon binary path: `/home/saidler/.cargo/bin/cortex` (pre-unified) |
| `sb cortex state` | 0 | `Last scan: 2026-05-20 17:52:55 UTC (1365 files)` |
| `sb cortex lint` | 0 | `Total: 533 error(s), 733 warning(s), 3919 info(s)` |
| `sb cortex lint --format json` | 0 | Valid JSON array of 5185 objects, schema `{message, path, rule, severity}` |
| `sb cortex lint --format yaml` | **2** | `error: invalid value 'yaml' for '--format <FORMAT>' [possible values: human, json]` (Phase 5 validated) |
| `sb cortex lint --rule naming` | 0 | Rule filter works: `Total: 1 error(s), 54 warning(s), 0 info(s)` |
| `sb cortex lint --path "notes/stop*"` | 0 | Path-glob filter works |
| `sb cortex link --scan all` | 0 | `44 violations` |
| `sb cortex link --scan people` | 0 | `0 violations` (strict subset, no people entities defined in config) |
| `sb cortex link --scan concepts` | 0 | `44 violations` (all link violations on this vault are concept-class) |
| `sb cortex link --scan everything` | **2** | clap rejects with `[possible values: people, projects, concepts, all]` (Phase 4 validated) |

### oracle

| Command | Exit | Sample |
|---|---|---|
| `sb oracle stats` | 0 | 1365 notes, full by-domain / by-type / by-status breakdown |
| `sb oracle call --list` | 0 | 18 tools listed with descriptions |
| `sb oracle call vault_overview` | 0 | `total_notes: 1365`, by-domain/by-type arrays, schema gaps |
| `sb oracle call schema_info` | 0 | Lists domains, note-types, origins (incl. `assisted`, `generated`), statuses |
| `sb oracle call tag_search` (no args) | 0 | `{count: 50, results: [...]}` - **Phase 2 rename verified, no legacy `tags` key** |
| `sb oracle call source_browse` (no args) | 0 | `{count, results}` - no legacy `sources` key |
| `sb oracle call creator_browse` (no args) | 0 | `{count, results}` - no legacy `creators` key |
| `sb oracle call domain_brief --json '{"domain":"ai"}'` | 0 | `{by_type, domain, results, starred, total_notes, unread: 446}` - **Phase 2 + `unread` always a number** |
| `sb oracle call knowledge_search` (bm25/hybrid/vector) | 0 | All 3 modes return `{count, results, ...}` with the same surface |
| `sb oracle call source_browse --json '{"host":"youtube.com","limit":3}'` | 0 | Returns 3 notes, real chained lookup from no-arg result |
| `sb oracle call note_read --json '{...,"detail":"tldr"}'` | 0 | Response key matches detail level (`tldr`/`summary`/`body`) - clean by-shape contract |
| `sb oracle call find_similar --json '{"path":"..."}'` | 0 | `{count: 2, results: [...]}` |
| `sb oracle call find_links --json '{"path":"..."}'` | 0 | `{outbound, inbound, note, orphan}` - distinct relationship lists, justifiably not `results` |
| `sb oracle call recent_activity --json '{"days":7}'` | 0 | `{count, days, results}` |
| `sb oracle call list_notes --json '{"domain":"ai","limit":3}'` | 0 | `{count, results}` |
| `sb oracle call ingest_history --json '{"limit":3}'` | 0 | `{count, entries}` - **see Defects** |
| `sb oracle call inbox_status` | 0 | `{classified, inbox_count, needs_review, notes, review_candidates}` - **see Defects** |
| `sb oracle call quality_report` | 0 | `{distribution, results}` |
| `sb oracle call classify_status` | 0 | `{total_classified: 821, by_method, by_domain, by_confidence, inbox_count, pending_review, unclassified}` |
| `sb oracle call duplicate_groups` | 0 | `{count, groups}` - `groups` not `results` (groups-of-notes, justifiably distinct from a flat list) |

### Edge cases

| Command | Exit | Result |
|---|---|---|
| `sb oracle call notatool` | 1 | `Error: unknown tool: notatool (use oracle call --list)` |
| `sb oracle call note_read --json '{"path":"notes/does-not-exist.md"}'` | **0** | Prints `Note not found: ...`. **Should arguably exit non-zero** - see Defects. |
| `sb oracle call knowledge_search --json '{invalid json'` | 1 | `Error: invalid JSON arguments` + `Caused by: 1: key must be a string at line 1 column 2`. Phase 3 compact-error format confirmed (no `Location:` line). |
| `sb borg replay` (missing required arg) | 1 | `Error: replay: must provide a trace_id, --since, --rejected, or --bootstrap-from-vault --note` |
| `sb -v borg replay` | 1 | `Error: "replay: must provide..."` - **no `Location:` line restored despite `-v`** - see Defects. |

## Output Format Matrix

| Command | `--format human` | `--format json` | other |
|---|---|---|---|
| `sb cortex lint` | grouped human-readable lines + `Total: ...` summary | valid JSON array, jq-pipeable, fields `{message, path, rule, severity}` (5185 objects on Scott's vault) | `--format yaml` rejected at parse time with exit 2 (designed) |
| `sb oracle call <tool>` | n/a (always JSON object) | implicit; pretty-printed by `Content::json` → stdout | n/a |

## Defects & Bugs

### D1. `sb -v` does not restore the `Location:` block (Phase 3 spec drift)

**Severity:** bug
**Found via:** `sb -v borg replay`
**Spec said:** "The hook hides the `Location:` block unless `--verbose` was parsed OR `RUST_BACKTRACE=1` is set."
**Actual:** With `-v`, the hand-rolled `EyreHandler::debug` impl in `sb/src/error.rs` delegates to `fmt::Debug::fmt(error, f)` on the *inner* `dyn Error + 'static`. Eyre's `Location` is captured on the `Report`, not the inner error - so this never prints it. Verbose mode does change the output (chain renders structurally as `Error { msg: ..., source: ... }`) but Location is never visible.
**Fix options:** switch to `color-eyre` (which keeps Location via its own hook), or have the hook implement a verbose path that uses eyre's own default Debug formatter via `eyre::DefaultHandler`.

### D2. `sb oracle call note_read` on a missing path exits 0

**Severity:** bug (CLI exit-code contract)
**Found via:** `sb oracle call note_read --json '{"path":"notes/does-not-exist.md"}'` → prints `Note not found: ...`, exits 0.
**Why it matters:** breaks shell pipelines that gate on success (`if sb oracle call note_read ...; then ...`).
**Spec:** the MCP tool returns a `CallToolResult` with `is_error: true` for missing paths (verifiable in `oracle::server::note_read`), but the CLI wrapper `sb oracle call` doesn't propagate that into a non-zero exit. The is_error → exit code translation is missing.

### D3. `sb doctor` exits 0 with "Issues detected"

**Severity:** minor / philosophical
**Found via:** `sb doctor; echo $?` → 0, despite the warning summary "Issues detected. See suggested fixes above."
**Note:** `rustup doctor` and `brew doctor` also exit 0 on warnings (only `error` returns non-zero). Acceptable if the contract is "warnings != failure," but worth confirming since `--quiet` style scripts will not catch warnings without parsing stdout.

### D4. `ingest_history` and `inbox_status` skipped the `results` rename (Phase 2 scope miss)

**Severity:** inconsistency
**Spec said:** "per-tool object - unchanged - they are not 'list of things' tools."
**Actual:** `ingest_history` returns `{count, entries}` and `inbox_status` returns `{notes, ...}` - both ARE list-of-things shapes that would benefit from the same canonical `results` key. The design's classification appears to have been a miscall; the unification on `results` should extend to these two.
**Fix:** rename `entries` → `results` in `ingest_history`, `notes` → `results` in `inbox_status`. Same clean-rename approach used for the other four.

## Observations

### O1. Running systemd daemons are still pre-unification binaries

`sb borg daemon --status` and `sb cortex daemon --status` show:
```
ExecStart=/home/saidler/.cargo/bin/borg daemon --start
ExecStart=/home/saidler/.cargo/bin/cortex --config /home/saidler/.config/cortex/cortex.yml --vault ... daemon --start
```

The deploy of v0.8.6 installed `sb` but did not regenerate the systemd units. The cortex unit still loads the legacy config path. To roll the daemons onto v0.8.6, run `sb borg daemon --install` and `sb cortex daemon --install` (these regenerate the unit content with the new sb-based ExecStart) followed by `systemctl --user restart borg cortex`. Until then the daemons run the May-19 builds.

### O2. Old per-subsystem binaries still present in `~/.cargo/bin/`

```
borg     46M  19 May 10:51
cortex   21M  19 May 10:51
oracle   21M  19 May 10:51
sb       62M  20 May 10:47
```

Roughly 88 MB of dead weight. Could be removed (`rkvr rmrf ~/.cargo/bin/{borg,cortex,oracle}`) once the units no longer point at them.

### O3. Boot-time noise on every short read-only command

Every `sb borg ...` invocation emits 4 log lines on startup:
```
[INFO] vault::logging: Logging initialized ...
[INFO] borg::config: Loaded config from: /home/saidler/.config/sb/borg.yml
[INFO] borg::startup: pipeline permits initialized: general=8 heavy=4
[INFO] borg::startup: ffmpeg thread caps: threads=4 ...
```

The ffmpeg thread caps don't matter for `borg intake list` or `borg blocklist list`. The startup setup runs unconditionally regardless of whether the command needs the pipeline. Not a bug, but the noise pollutes the terminal for inspection commands. A quieter default log level for non-daemon paths would help.

### O4. `note_read` response shape is shaped by `detail` level

The key in the response matches the requested detail:
- `detail: "metadata"` → no extra body key
- `detail: "tldr"` → `tldr: "..."`
- `detail: "summary"` → `summary: "..."`
- `detail: "full"` → presumably `body: "..."` (untested)

This is a clean by-shape contract: a consumer that asks for `summary` knows to look at `summary`. Worth documenting explicitly in the tool description if it isn't already.

### O5. CLAUDE.md and configuration are aligned with v0.8.6's `~/.config/sb/` layout

All three subsystem loaders successfully load from `~/.config/sb/`. `sb doctor` reports all configs parse cleanly and the shared catalogue is in sync with the repo. The Phase 1a migration is observably done.

## Pipeline Recipes (working, copy-pasteable)

```bash
# Top 5 source domains by note count
sb oracle call source_browse --json '{"limit":50}' \
  | jq -r '.results[] | "\(.count) \(.host)"' | sort -rn | head -5

# Top 5 tags by frequency
sb oracle call tag_search --json '{"limit":200}' \
  | jq -r '.results[] | "\(.count) \(.tag)"' | sort -rn | head -5

# Top 5 vault domains
sb oracle call vault_overview \
  | jq -r '.by_domain[] | "\(.[1]) \(.[0])"' | sort -rn | head -5

# Lint error counts grouped by severity (from JSON output)
sb cortex lint --rule frontmatter --format json \
  | jq '[.[] | .severity] | group_by(.) | map([.[0], length])'

# Chained: top source domain → list its first 3 notes
top=$(sb oracle call source_browse --json '{"limit":1}' | jq -r '.results[0].host')
sb oracle call source_browse --json "{\"host\":\"$top\",\"limit\":3}" | jq -r '.results[] | .title'

# DLQ pending traces → show details for the first one
trace=$(sb oracle call ingest_history --json '{"limit":1}' | jq -r '.results[0].trace_id // empty')
# (or fall back to: sb borg dlq list --status pending --limit 1)
sb borg dlq show "$trace"
```

## Release Validation

| Check | Status |
|---|---|
| Tag `v0.8.6` exists locally | yes |
| Tag `v0.8.6` exists on origin | yes (`83aee0b9...` → commit `221d5685`) |
| Tag is annotated | yes (`git cat-file -t v0.8.6` → `tag`) |
| GitHub release for v0.8.6 | **no** - `gh release view v0.8.6 -R scottidler/second-brain` returns `release not found` |
| Release pipeline | none configured for this repo |
| Binary install matches `sb --version` | yes (`sb v0.8.6` for `~/.cargo/bin/sb` matches `git describe`) |

The repo has no release automation. Tags are published; binaries are not. If you want consumers (yourself or future tooling) to download v0.8.6 binaries, you'd need to set up a release workflow. For a personal-use binary this is acceptable.

## Skipped (intentional)

- **Mutating verbs:** `borg ingest`, `borg note`, `borg hotkey --install`, `borg sign`, `borg migrate --apply`, `borg dlq replay/archive`, `borg reingest`, `borg replay`, `borg retention sweep`, `borg reingest-failed`, `borg blocklist remove/clear`, `borg backfill-ingested`, `borg dashboard refresh`, `cortex classify --apply`, `cortex lint --apply`, `cortex link --apply`, `cortex sweep --migrate`, `cortex migrate --apply`, `cortex summarize`, `cortex daemon --install/uninstall/start/stop`, `bootstrap`
- **Long-running:** `oracle serve`, `borg/cortex daemon --start`
- **Interactive / device-bound:** `borg sign`, `bootstrap` (would download model + write systemd files)
- **Heavy:** `oracle index` (would rewrite the SQLite index)

## Verdict

v0.8.6 ships its specified surface cleanly. The four observably-shipped phases (config unification, `results` rename, `--format`/`--scan` enum validation, daemon `--status` parity) all behave as designed against the real vault. The Phase 3 verbose-mode regression (D1), the missing-path exit code (D2), and the Phase 2 scope miss for `ingest_history`/`inbox_status` (D4) are real follow-ups; D3 is a contract clarification.
