# Design Document: `sb borg ingest` items are URLs or URL-list files

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-10-28
**Status:** In Review (the code landed ahead of this doc in 2212e14; see Implementation Plan)
**Review Passes Completed:** 2/5 (draft, correctness against the code and the executed criteria), plus review-panel round 1 (Staff Engineer only; the Architect seat failed twice)

## Summary

`sb borg ingest` takes 1+ items. Each item is an http(s) URL or a file holding URLs, and a list file expands to every URL in it. Content files (PDF, image, document, audio) move back behind an explicit `--file`. Malformed input is a loud exit 2 before anything is submitted (`--clipboard` keeps its exit 1, unchanged from v0.19.4).

## Problem Statement

### Background

- 2026-10-27, Scott, asking right after a bulk ingest of a scour's URL list: "does the cli allow you to supply more than one url? or a filepath?"
- Same day, as the ask for `docs/design/2026-10-27-multi-target-ingest.md`: "so we need this as a functionality 1+ items that can be urls or filepaths. when it is more than some config default (say 5) then it is a bulk ingest ..."
- 2026-10-28, Scott, clarifying: "I wanted this cli to take 1+ items where the items can be URLs|FILEs. you can easily tell the difference by running urlparse and file exists on each item. error on malformed. if you get a file it has a pile of urls in there, ingest them all."

### Problem

What shipped did not match the ask:

- v0.19.3 read a positional FILE as a document to ingest as a note (the old `--file`, which it removed). The URLs inside a list file were never read.
- v0.19.4 added `--from <FILE>` for lists: a second flag for what the positional item should have done.
- URLs were prefix-checked (`starts_with("http://"|"https://")`), not parsed, so `https://exa mple.com` passed the client and failed later at the daemon.
- Paths skipped tilde expansion, against the `CLAUDE.md` rule that every user path goes through `vault::paths::expand_tilde`.

### Goals

- Each item is a URL or a URL-list file, decided per item (Scott, 2026-10-28).
- A list file's URLs are all ingested (Scott).
- Malformed input is an error (Scott). It is reported before any submit, because a half-submitted list cannot be undone.
- Content files stay ingestible, behind a flag that says what they are (Scott picked option A, 2026-10-28).

### Non-Goals

- **Excluded:** recursion. A path inside a list file is an error, not followed.
- **Excluded:** list syntax beyond one URL per line (no markdown links, inline comments, or several URLs per line).
- **Excluded:** daemon-side URL validation (`borg/src/pipeline.rs`, still a prefix check). The client is the gate this doc owns.
- **Excluded:** de-duplication across lists. The ledger's existing duplicate handling applies per URL.
- **Parked:** markdown or text content ingest. If `.md`/`.txt` ever become ingestible as notes, `--file` already carries that, and nothing in the positional grammar changes.

## Proposed Solution

### Overview

```
sb borg ingest [OPTIONS] [TARGETS]...

  TARGETS        http(s) URLs, or files listing one URL per line
                 (blank and `#` lines skipped; `-` reads the list from stdin)
  --file <FILE>  ingest this local file as a note (image, pdf, document, audio); repeatable
  --clipboard    one URL from the clipboard; conflicts with TARGETS and --file
  -t/--tags      greedy as before; a tag containing `://` or naming an existing path is a swallowed target (exit 2)
  --force, --note   unchanged (--note with --file is an error)
```

### Architecture

Every TARGETS/`--file` run goes through one preflight, `borg::classify_targets` (`borg/src/client/targets.rs`). `--clipboard` skips it and checks its one URL with the same `is_http_url` before submitting. It turns every input into concrete `Target`s or returns one `UsageError`. `sb` maps that error to exit 2, so nothing reaches the daemon or the local pipeline.

Per positional item, in order:

1. Contains `://` or passes `is_http_url` -> must pass `is_http_url`, else "malformed URL". A URL-shaped item never falls through to a file lookup.
2. `-` -> read stdin once (a second `-` is an error) and parse it as a list.
3. `expand_tilde(item)` is a regular file:
   - content extension (`assets::is_{image,pdf,document,audio}_extension`) -> error "a content file; use --file".
   - else read it as UTF-8 and parse it as a list.
4. It exists but is not a file -> error. Otherwise -> "not an http(s) URL and no such file".

List parsing (`parse_url_list`): trim each line, then skip blank lines and `#` lines. Every other line must pass `is_http_url`, or it is reported as `<source>:<line>: <text>`. A list with no URLs is an error. The URLs expand in place, in input order.

Each `--file`: after `expand_tilde` it must be a regular file with a content extension, else an error.

All errors (swallowed tags, `-` twice, `--note` with `--file`, every bad item, list line and `--file`) are collected and reported together in one `UsageError`, then the run stops. The preflight checks that a `--file` exists and has a content extension; a file that cannot be read still fails later, in the run, after the URLs are submitted.

### Data Model

No change. `Target::{Url, File}`, `IngestRun`, receipts, and the batch protocol are untouched. The bulk rule (`is_bulk`) counts the expanded URLs plus the `--file` targets, and needs at least one URL, as before.

### API Design

- `pub fn is_http_url(s) -> bool`: true only with all of the following.
  - a literal `http://`/`https://` prefix (any case)
  - no whitespace, control character, or backslash
  - `url::Url::parse` succeeds with a non-empty host

  `Url::parse` alone is too forgiving: it percent-encodes spaces and normalizes `http:host`. A bare `%` passes, as in a browser. IDN hosts, IPv6 literals, ports and userinfo pass (pinned in the test table). `https:///host` passes because the url crate reads it as `https://host`. `--clipboard` (`resolve_ingest_url`) uses the same check.
- `pub fn parse_url_list(text, source) -> Result<Vec<String>, UsageError>`
- `pub fn classify_targets(items, files, tags, note, read_stdin) -> Result<Vec<Target>, UsageError>`. `read_stdin` is a closure so tests never touch the process's stdin.
- Removed: `--from`, `read_target_lists`, `parse_target_list`.

### Implementation Plan

#### Phase 1: Item grammar, `--file`, strict URLs
**Model:** opus
**State:** landed in 2212e14 before this doc, so the review panel audits it against the doc.
- `borg/src/client/targets.rs`: `is_http_url`, `parse_url_list`, the `classify_targets` rules above, and the `assets` content check.
- `borg/src/client.rs`: `resolve_ingest_url` uses `is_http_url`.
- `borg/src/lib.rs`: export `is_http_url` and `parse_url_list`; drop `read_target_lists` and `parse_target_list`.
- `sb/src/cli/borg/args.rs`: drop `from`, add repeatable `file`, set `required_unless_present_any = ["clipboard", "file"]`, and make `clipboard` conflict with `targets` and `file`.
- `sb/src/cli/borg.rs`: one `classify_targets` call that gets stdin through a closure.
- Round-1 fold-in (2026-10-28): every preflight error is collected instead of three early returns; the bare-`%` rule is dropped; IDN, IPv6, userinfo and bare-`%` rows join the URL table.
- Tests: `borg/src/client/targets/tests.rs` (classification section rewritten) and `sb/tests/ingest.rs` (list expansion, a bad last line, removed `--from`).
- Docs: `sb/AGENTS.md`, `borg/AGENTS.md`, and the follow-up section of `docs/design/2026-10-27-multi-target-ingest-implementation-notes.md`.
- **Success criteria:**
  - `cargo test --workspace --lib client::targets` passes, including `one_bad_list_line_fails_the_whole_run_naming_file_and_line`, `a_tilde_list_path_is_expanded` and `dash_reads_one_url_list_from_stdin_once`.
  - `sb/tests/ingest.rs` `a_malformed_last_list_line_exits_2_with_zero_posts` passes.
  - `every_preflight_error_is_reported_together` passes.
  - Proof the tests bite (2026-10-28, each file restored after): with `parse_url_list` accepting any line, `one_bad_list_line_fails_the_whole_run_naming_file_and_line` fails; with the `--note` check returning early, `every_preflight_error_is_reported_together` fails.

#### Phase 2: Release and prove it live
**Model:** sonnet
- `bump release`, then `otto deploy` on ripr. Needs Scott's go.
- Run `sb borg ingest <list-file>` against the live daemon with a small list of new URLs, then `sb borg wait`.
- **Success criteria:**
  - `sb --version` and `/health` report the new version.
  - Every URL in the list prints `-> queued` and `sb borg wait` exits 0.

## Acceptance Criteria

Each criterion was run on main at 2212e14 (`target/debug/sb`, built by `otto ci`). The fixtures are a `urls.txt` whose line 3 is `https://exa mple.com`, and a 1-byte `a.pdf`.

- [ ] A list with one bad line exits 2, naming `file:line`, and submits nothing.
  - `sb borg ingest urls.txt; echo $?`
  - Observed on main: `Error: cannot ingest: .../ac/urls.txt:3: https://exa mple.com (not an http(s) URL; a list holds URLs only)`, then `exit=2`. Zero submits are pinned by `sb/tests/ingest.rs::a_malformed_last_list_line_exits_2_with_zero_posts` (stub daemon, `bodies` empty).
- [ ] A positional content file exits 2 and points at `--file`.
  - `sb borg ingest a.pdf; echo $?`
  - Observed on main: `Error: cannot ingest: .../ac/a.pdf (a content file; use --file .../ac/a.pdf to ingest it as a note)`, then `exit=2`.
- [ ] `--from` is gone.
  - `sb borg ingest --from urls.txt; echo $?`
  - Observed on main: `error: unexpected argument '--from' found`, then `exit=2`.
- [ ] Help shows the new surface.
  - `sb borg ingest --help | grep -E -- '--file|--from|TARGETS'`
  - Observed on main: the `[TARGETS]...` line says "http(s) URLs, or files listing one URL per line"; `--file <FILE>` says "Ingest this local file as a note (image, pdf, document, audio). Repeatable"; no line matches `--from`.
- [ ] Live: a list file ingests every URL in it.
  - `sb borg ingest <list>` against the deployed daemon.
  - Not run yet: it depends on Phase 2's release.

## Resolved Decisions

- **2026-10-28, Scott: option A.** A positional file is always a URL list, and content files take `--file`.
  - The Architect (Gemini) and the Staff Engineer (Codex) recommended A.
  - Fable recommended B, an extension-based split (see Alternatives).
- **2026-10-28, all three reviewers: drop `--from`.** It shipped one day earlier (v0.19.4), and nothing in `bin/`, `.otto.yml`, `borg/clients/`, skills or agents calls it.
- **Strict validation before any submit, collected and reported together.** This follows from Scott's "error on malformed"; all three reviewers agreed.
- **The bulk threshold counts URLs after expansion.** All three reviewers agreed, and `is_bulk` already counts that way (plus `--file` targets).
- **2026-10-28, panel round 1 (Staff Engineer): every preflight error is reported together.** The doc promised it and the code returned early on three checks; the code was fixed, not the doc.
- **2026-10-28, panel round 1: a bare `%` passes.** The Staff Engineer listed it as defer. Closed instead of deferred: browsers accept `?q=100%`, Scott's "error on malformed" targets items that are not URLs, and rejecting it would break hotkey captures of real pages.
- **2026-10-28, panel round 1: `-t` keeps the `://` match.** A malformed URL swallowed by `-t` is still a swallowed target; documented as a breaking change rather than narrowed to `is_http_url`.

## Alternatives Considered

### Alternative 1: Decide by extension, no flag (Fable)
- **Description:** A file with a content extension is ingested as a note, and any other file is a URL list.
- **Pros:**
  - No new flag.
  - The extension sets already decide content vs rejected inside `ingest_file`.
- **Cons:**
  - An item's meaning depends on a list of extensions that can grow.
  - Adding `md` or `txt` content ingest later would silently turn list files into notes.
- **Why not chosen:** Scott picked A, and the positional rule then matches his words exactly.

### Alternative 2: Keep `--from` beside positional files as content (v0.19.4)
- **Description:** Leave the shipped design.
- **Why not chosen:** It is the mismatch this doc fixes. Two ways to pass a list, and the positional one does the wrong thing.

### Alternative 3: Content sniffing (a file is a list if every line parses as a URL)
- **Why not chosen:** A list with one typo becomes indistinguishable from a document, so the error cannot say which thing it judged. All three reviewers rejected it.

## Technical Considerations

### Dependencies
- The `url` crate is already in the workspace, and borg already uses it.
- No new dependencies.

### Performance
- Lists are read fully into memory before any submit. A list of thousands of URLs is a few hundred KB.

### Security
- No new surface. The client reads local files the operator names, and URLs are validated before they leave the host.

### Testing Strategy
- Unit tests cover:
  - the validator, with good and bad URL tables
  - list expansion order, `file:line` errors, and empty lists
  - URL-shaped items that never fall through to a file lookup
  - tilde paths, stdin `-` only once, and `--file` accept and refuse
  - tag swallowing, including a malformed URL tag
- CLI tests run the built binary against a stub daemon and assert zero POSTs on any usage error.

### Rollout Plan
- Ship the single binary with bump, then `otto deploy`.
- **Breaking changes vs v0.19.4:**
  - `sb borg ingest report.pdf` becomes `sb borg ingest --file report.pdf`.
  - `--from` is gone.
  - A positional non-content file (`notes.md`) used to fail as "Unsupported file type" with a junk `intake-rejected` receipt; now it is read as a URL list and fails with `file:line`, and no receipt is written.
  - A URL with whitespace, a control character or a backslash used to be submitted and fail at the daemon; now it exits 2 (TARGETS) or 1 (`--clipboard`).
  - A `-t` tag containing `://` (even a malformed one, like `ftp://x`) is now a swallowed target, exit 2. v0.19.4 matched only `http(s)://` tags.
- Blast radius:
  - The hotkey runs `--clipboard`, so a copied URL with whitespace or a backslash now fails with exit 1 and no toast. A normal copied URL, bare `%` included, is unaffected. The bundled script passes one literal URL.
  - The extension POSTs JSON straight to `/ingest`, so it is unaffected.
  - The `intent-guard.sh` INGEST rule that referenced `--from` was removed in scottidler/claude 96bfc71.
- Ship order: this repo only.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| A muscle-memory `sb borg ingest x.pdf` | Med | Low | Exit 2 with "use --file <path>" in the message |
| A list filename containing `://` | Low | Low | It is treated as a URL and errors; rename the file (a `./` prefix does not help, the name still contains `://`) |
| A strict validator rejects a URL the daemon would have taken | Low | Low | Only whitespace, control characters and backslashes are rejected beyond `Url::parse`; the error names the URL |
| A file changes between preflight and submit | Low | Low | The list is read once in preflight; the URLs submitted are the ones validated |

## Open Questions

None.

## References

- `docs/design/2026-10-27-multi-target-ingest.md` and its implementation notes
- Commit 2212e14 (the implementation)
- Reviewer briefs and outputs (Fable, Architect, Staff Engineer), 2026-10-28, in this session's scratchpad
