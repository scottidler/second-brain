# Vault reingest and borg crash fixes

Branch: main

## Next action

Wait for the older-YouTube reingest to finish, then run the duplicate check and collapse any sets it finds (step 1 under "Open work"). Before any further bulk rewrite, settle which host Syncthing device `765W75U` ("desk") is and stop its cortex (step 2), or conflict copies keep appearing.

## Read first, in order

1. `docs/design/2026-10-14-note-sources-intent-tools-channels.md` (the v0.17.0 feature doc; Status: Implemented)
2. `docs/design/2026-10-14-note-sources-intent-tools-channels-implementation-notes.md` (phase notes plus the round-1 implementation audit)
3. `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md:151` (the "--force forks" goal that 2667c2e reversed; the doc is NOT yet amended)
4. `git log --oneline 4a5a8d0..HEAD` (every commit below)

## State at hand-off (2026-10-15 ~19:30 PDT)

- Host: this machine's hostname is `ripr` (10.10.10.11). It runs the borg and cortex daemons. `~/.claude/WHOAMI.md` still says ripr runs no daemons and is wrong.
- Released: v0.17.0, an annotated tag on e7cdde5, pushed.
- Deployed locally, NOT pushed or tagged: `sb v0.17.0-5-gfa0a2e0` (main is 5 commits ahead of origin). Check: `cd ~/repos/scottidler/second-brain && git rev-list --count @{u}..HEAD; sb --version`
- Pushing these needs Scott's approval (`bump release`, a patch, run bare, in the background).

### Commits after the v0.17.0 tag (local, deployed)

| sha | what |
|---|---|
| 22cdcd5 | `process_url` rejects non-http(s) schemes as intake-rejected; reingest skips `clyde://`; `--before`/`--after` made inclusive; reingest output shows the note path instead of "Untitled" |
| c7e5eca | `sb borg reingest --jobs N` (concurrent, events in completion order) |
| 2667c2e | forced harvest / `replay --from-stage 2` REPLACE by default (fresh note under its new id, older notes with the same `source:` go to rkvr); `--fork` keeps both |
| 57babd7 | reingest sends each source once (ledger has a row per reingest) |
| fa0a2e0 | stack-overflow crash fix: readability only takes text/html responses; `render_markdown` refuses nesting deeper than 256 before `htmd` |

### Dotfiles commits (local, not pushed): `~/repos/scottidler/dotfiles`

- d502ddd: borg pipeline caps 64 traces / 32 heavy (`HOME/.config/sb/borg.yml`)
- 190f139: synced v0.17.0 distill patterns
- 66c37c1: `HOME/.config/yt-dlp/config` (Firefox cookies + `--remote-components ejs:github`), symlinked to `~/.config/yt-dlp`
- 118893c: `systemd-coredump` in the apt list in `manifest.yml` (only that hunk; Scott's other manifest edits are uncommitted and not ours)

### Host changes outside any repo (session-scoped facts that persist)

- `~/.config/systemd/user/borg.service.d/core.conf`: `LimitCORE=infinity` (needed for coredumps; the soft limit was 0)
- `systemd-coredump` installed by Scott; it replaced `apport-core-dump-handler`
- GNOME Do Not Disturb is ON (`gsettings get org.gnome.desktop.notifications show-banners` -> `false`). Turn it back on with `gsettings set org.gnome.desktop.notifications show-banners true` once the reingest is done.

## What was done to the vault

- Every URL note ingested 2026-09-16..10-15 was reingested; every session thread since 2026-07-01 was re-harvested (228 threads, replace mode).
- Pre-2026-09-16 articles (457) and GitHub repos (112) were reingested.
- Duplicates collapsed twice by a scratch script (freshest content kept at the oldest filename; the rest went to rkvr, recoverable for 3 weeks under `/var/tmp/rmrf/`).
- 24 session notes that a `clyde://` URL reingest had overwritten were restored from their `hv-*` staged transcripts.

## Open work

1. **Background job still running:** `sb borg reingest --type youtube --before 2026-09-15 --jobs 4` (1,120 sources; 181 replaced and 4 failed when this was written). It belongs to the old session's shell and may be gone. Check with `pgrep -af '[s]b borg reingest'`. If it is gone, re-run the same command; each source is sent once, and the YouTube notes it already replaced are simply redone. Keep `--jobs` at 4 or below: a burst of 60 tripped YouTube's bot check earlier, and the cookies now make these requests run as Scott's Google account.
2. **Syncthing conflict copies:** 24 sets appeared during the re-harvest. Version vectors show edits from devices `765W75U` ("desk") and `OMQMXHE` ("ltl-7007") at 18:2x while ripr was rewriting the same notes. Probe: `K=$(rg -o '<apikey>([^<]+)' -r '$1' ~/.local/state/syncthing/config.xml); curl -s -H "X-API-Key: $K" "http://127.0.0.1:8384/rest/db/file?folder=obsidian&file=<path>"`. Unanswered question to Scott: which host is device "desk" now (`desk.lan` no longer resolves; Scott said desk is now talos.lan), and does it still run cortex? Rec: stop its cortex/borg so ripr is the only vault writer.
3. **Conflict-file renaming bug:** 26 of 30 conflict copies were renamed from `….sync-conflict-…-ID3CBXW.md` to `…sync-conflict-…-id3cbxw.md` (lowercased, dot dropped), which makes them look like ordinary notes. The renaming is suspected to be cortex's filename hygiene; this is not verified. Find the renamer and make it skip `*.sync-conflict-*`. Probe: `fd -t f -i 'sync-conflict' ~/repos/scottidler/obsidian | head`
4. **Collapse duplicates again** after items 1-2. Count: rebuild the scratch planner (group notes/ and inbox/ by `source:`, keep the max `ingested:` content at the min-`ingested:` path, rkvr the rest). It lived in the old session's scratchpad and is gone.
5. **Amend the design doc** `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md:151`: forced re-harvest now replaces; `--fork` keeps both (2667c2e).
6. **4 YouTube failures** in the older run: read them with `sb borg log --status failed --since 1d --source '%youtube%'`. One known permanent failure: `i62hPoC9W34` (the video was removed upstream).
7. **Release** the 5 local commits plus a patch bump, with Scott's approval. Push the dotfiles commits too.

## Known facts that save time

- The bulk-ingest guard (`intent-guard.sh`) denies a quoted URL, a pipe, or a loop on `sb borg ingest`. Run one bare literal URL per command, or prefix `BULK_INGEST_ORDERED_BY_SCOTT=<n>`.
- Restarting borg (`otto deploy`, `systemctl --user restart borg`) leaves clients polling dead traces for up to 31 min until the watchdog marks them `crashed`. Stop reingest clients before restarting.
- Crash debugging: `coredumpctl list`, then `coredumpctl debug <pid> --debugger=gdb --debugger-arguments='-batch -ex "bt -25"'`.
- `harvest` holds an exclusive lock and publishes threads one at a time. A second harvest, or a session replay, cannot run alongside it.

## Suggested skills

- `how-to-execute-a-plan` is not needed. This is operations work, not a phased doc.
- `bump` / shipit path for the release (repo is ungated: `bump release` on main).
- `handoff` (resume mode) for the receiver.
