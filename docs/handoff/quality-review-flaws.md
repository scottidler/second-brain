# Handoff: second-brain quality review fixes

Branch: quality-review-fixes

## Next action

Run the implementation audit: `Skill(review-panel)` with `DOC_PATH=/home/saidler/repos/scottidler/second-brain/main/docs/design/2026-10-05-quality-review-fixes.md`, scope "implementation audit", head `5989ddd` (or the current HEAD if this handoff was committed on top). The first dispatch never ran: the Bash sandbox bridge in the prior session died ("Linux sandbox HTTP bridge socket is missing"), and both reviewer seats failed to launch. Scott chose to restart Claude Code rather than run the seats unsandboxed. Then fold the findings into fix commits, then stop at the finalization checkpoint (`/how-to-execute-a-plan` step 3) and get Scott's approval before any bump, push, tag, PR, or install.

## Read first

1. `docs/design/2026-10-05-quality-review-fixes.md`: the plan (Phases 0-31, Status Implemented), Acceptance Criteria with verified results, Addendum D (Phase 0 spikes), Addendum E (Scott's decision on receipts test isolation).
2. `docs/design/2026-10-05-quality-review-fixes-implementation-notes.md`: per-phase decisions, deviations, break-it records, and every "Orchestrator" section (out-of-plan fixes and carried findings).
3. `git log --oneline 01408d3..HEAD`: 38 commits, one per phase plus the orchestrator fixes below.

## State at time of writing

- Branch `quality-review-fixes`, cut from `main` at `01408d3` (v0.15.13). Nothing pushed, no PR, no tag, no install.
- All phases 0-31 committed; Phase 0 had no code. `otto ci < /dev/null` green at `5774e14`: 3,059 passed, `source-lint: clean`.
- `5989ddd` flips the doc to Implemented and records acceptance criteria 1-4 as verified. Criterion 5 (desk: no unit drift, `graph --rebuild` keeps fact edges, hotkey binds) is UNVERIFIED until a deploy.
- Out-of-plan commits on the branch (each recorded in the notes):
  - `9bd3337` wait closed-port flake; `8cd6797` wait timing bounds tied to the request-timeout cap
  - `a9c617d` receipts test isolation + WAL-conversion retry + test-only sandbox guard (two tests opened the live `~/.local/share/sb/borg/receipts.db`)
  - `61dd77e` youtube frame test per-run tempdir
  - `7991e37` closed-port test class: `vault::testnet::closed_port()` + source-lint check
  - `da59d7c` the original version of this handoff (committed by an unidentified writer between Phases 8 and 9)

## Blockers

- Sandbox bridge (session-scoped): re-test with any trivial sandboxed command, e.g. `echo ok`. If it errors with "HTTP bridge socket is missing", the restart did not fix it; report to Scott, do not force `dangerouslyDisableSandbox` on the reviewer scripts (they are in `sandbox.excludedCommands`; CLAUDE.md forbids it).
- Review-panel round count: the failed dispatch (run dir `/tmp/review-panel/NXcIoLGX/`, may not survive a reboot) may count as round 1 against `panel-round-guard.sh`. If a re-dispatch is denied, report the denial text verbatim to Scott.

## Open for Scott (raise at the checkpoint)

- `/proc/*/environ` guard in `scottidler/claude` `secret-echo-guard.sh` (`:506` list lacks it; `ps e` too): A (deny + tests) vs B (A + names-only helper). Rec A. Unanswered. Cause: a Phase 2 worker dumped a child env and leaked the home GitHub PAT value into its subagent transcript; rotation is Scott's call.
- Ship shape: the doc plans one PR per track (8 tracks); the branch holds all tracks in sequence.
- Carried pre-existing findings (not fixed, out of scope): incremental graph pass drops a changed entity hub's fact edges; quality lint counts a self-link as inbound while `recompute_inbound_link_counts` does not; `/tmp/borg-youtube-frames` leak (2.6G, 55 dirs); `cargo build -p borg --tests` alone fails (borg tests rely on workspace feature unification for `vault::search`); pin CPU torch in `bin/gen-bge-reference.py`.
- Addendum E follow-up: inject the receipts location instead of resolving it from `XDG_DATA_HOME` (preferred design, deferred by Scott).

## Rollout steps after deploy (Scott's, from the doc's Rollout Plan)

- `sb cortex graph --rebuild` with a fact-edge count before and after
- `sb borg daemon --install`, `sb cortex daemon --install`, daemon-reload, restart, then `sb doctor` from the installed binary (a `target/debug/sb` run reports drift on every unit because ExecStart embeds the binary path)
- `sb borg hotkey --install`, then a capture
- re-sign the extension, check on lappy
- remove `fabric-patterns:` from `~/.config/sb/cortex.yml:96`

## Session-scoped notes

- `/tmp` tmpfs was ~89% full (other sessions' sandbox dirs plus the youtube-frames leak); put scratch under `~/.cache`.
- This shell rewrote a bare argument equal to a repo dir name (`borg`, `vault`) into an absolute path; run such commands via `bash -c 'cd <repo> && ...'`.
- Always run `otto ci < /dev/null`.

## Suggested skills

- `review-panel` (next action), then `how-to-execute-a-plan` finalization steps 3-7
- `release-driver` / `shipit` only after Scott approves at the checkpoint
- `babysit` for any PR opened
