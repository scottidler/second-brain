# Design Document: Extension installer cleanup for the post-snap world (model A)

**Author:** Scott Idler (with Claude)
**Date:** 2026-06-07
**Status:** Implemented (code: Phases 1-3; Phase 4 is manual operator bootstrap)
**Review Passes Completed:** 5/5 + Architect review (Gemini)

## Summary

snap Firefox is abandoned on every machine (removed + apt-pinned, replaced by
Mozilla's `/opt` build via the dotfiles `firefox-opt` manifest script - see
`docs/postmortems/2026-06-07-firefox-snap-breaks-borg-capture.md`). The capture
web-ext installer (`borg/src/extension/install.rs`) still carries snap-specific
code that is now dead weight and a footgun, and `otto deploy`'s extension step
silently no-ops on every machine. This makes the `/opt` tarball + enterprise-policy
force-install the **first-class, only-supported** path, converts snap detection
into a **loud actionable error** (not silent mis-install), removes the snap code,
and bootstraps the force-install so `otto deploy` actually refreshes the extension.

## Problem Statement

### Background

The extension installer was written when desk/lappy ran **snap** Firefox. Snap
confines Firefox so it cannot read a system `policies.json`; the installer
therefore special-cased snap by copying the signed `.xpi` directly into the snap
profile's `extensions/` dir (`InstallStrategy::ProfileExtension`). `detect_firefox()`
checks `is_snap_firefox()` (via `snap list firefox`) **first** and returns
`FirefoxInstall::Snap`, shadowing everything else.

As of 2026-06-07, snap Firefox is gone everywhere: removed, apt-pinned out, and
replaced by Mozilla's tarball under `/opt/firefox` (codified as the dotfiles
`firefox-opt` script). The capture extension only works on this non-snap build.

### Problem

1. **Dead, misleading code.** `FirefoxInstall::Snap`, `InstallStrategy::ProfileExtension`,
   `is_snap_firefox()`, `snap_active_profile_dir()`, and the snap bail in
   `policy_path()` exist solely to serve a configuration we have deliberately
   eliminated. They add surface area and confuse the control flow.
2. **`otto deploy` no-ops silently.** The deploy hook runs
   `sb borg extension install --no-policy --if-installed`. On `/opt` Firefox the
   extension is not (yet) force-installed via `/opt/firefox/distribution/policies.json`,
   so `--if-installed` skips with `extension not installed on this machine; skipping`.
   Deploy never refreshes the extension - the "no-op bullshit."
3. **Removing snap handling naively fails *silently*.** This is the load-bearing
   constraint. `which firefox` on a snap box returns `/usr/bin/firefox`, a
   shell-script wrapper (not a symlink into `/snap/`), so `std::fs::canonicalize`
   cannot follow it. If `is_snap_firefox()` is simply deleted, `detect_firefox()`
   falls through to the `/usr/bin/` branch and returns `AptOrDeb`, writing
   `/etc/firefox/policies/policies.json` - which snap Firefox cannot read.
   Result: the extension silently never loads. So snap **detection must stay**;
   only the snap **install path** is removed, replaced by a hard error.

### Goals

- Make `/opt` Tarball + enterprise `policies.json` force-install the first-class,
  only-supported install path.
- Detect snap Firefox and **fail loudly** with an actionable message ("install
  Mozilla `/opt` Firefox via `manifest -s firefox-opt`"), never silently
  mis-install to a policy file snap cannot read.
- Remove the snap install machinery (`ProfileExtension`, `snap_active_profile_dir`,
  the snap `policy_path` bail), keeping only snap *detection* (now → error).
- Make `otto deploy`'s extension refresh actually work: once force-installed,
  every deploy re-signs the `.xpi` and the policy keeps Firefox pointed at it.
- Bootstrap the force-install on desk + lappy.
- Update `borg/src/extension/install/tests.rs` to drop snap-path tests and add a
  snap-detection-errors test.

### Non-Goals

- AMO listed/unlisted distribution or auto-update changes (force-install via
  policy is the chosen model; AMO auto-update is model B, rejected).
- The manual Firefox-Sync re-add of the extension (force-install replaces it;
  the user does not add it via `about:addons` anymore).
- flatpak and apt/deb support changes - they keep working unchanged
  (`policies.json` force-install already covers them).
- macOS/Windows (installer is already Linux-only; `sign` + manual install there).

## Proposed Solution

### Overview

Keep snap *detection* but make snap a terminal error. Make the Tarball/apt/flatpak
`policies.json` force-install the only real install path. Bootstrap the policy on
each machine so `--if-installed` deploys refresh it.

### Architecture

`detect_firefox()` after cleanup:

1. If `is_snap_firefox()` → `FirefoxInstall::Snap` (retained purely so the
   consumers can render a precise error; see below).
2. Else resolve `which firefox` → canonicalize → hand the resolved string to a
   new pure helper `classify_firefox_path(resolved: &str) -> FirefoxInstall`:
   - `/opt/firefox/...` → `Tarball("/opt/firefox")`
   - `/usr/bin/...` or `/usr/lib/...` → `AptOrDeb`
   - flatpak markers → `Flatpak`
   - else → `Unknown`

**Why split out `classify_firefox_path`:** today the path-classification logic
is inlined in `detect_firefox()`, which also shells out to `which`/`snap`. That
makes the classification untestable - the result depends on whatever Firefox the
test host happens to run. Phase 2's regression guard against the
canonicalize-wrapper trap (`/opt/firefox/...` → `Tarball`, `/usr/bin/...` →
`AptOrDeb`) **cannot be written deterministically** without a pure function that
takes a string and returns a `FirefoxInstall`. `detect_firefox()` becomes a thin
shell: snap probe → `which` → canonicalize → `classify_firefox_path`. The I/O
stays in `detect_firefox`; the logic-under-test moves to the pure helper.

`install_strategy()` / `policy_path()`:

- `Tarball | AptOrDeb | Flatpak` → `PolicyFile { path }` (force-install via
  `force_installed` ExtensionSettings; unchanged).
- `Snap` → `eyre::bail!` with the migration message (replaces the old
  `ProfileExtension` branch and the old snap `policy_path` bail). One message,
  one place.
- `Unknown` → existing "could not detect" bail.

`run()` behavior on snap:

- **Explicit install** (`sb borg extension install`, no `--if-installed`): snap →
  hard error. The operator asked to install; tell them why it can't and what to do.
- **Deploy hook** (`--if-installed`): snap → **warn + skip** (return
  `skipped_not_installed: true`), so `otto deploy` does not fail on a box that
  still has snap. The warning names the remedy. Rationale: `--if-installed` means
  "only act if this machine already has the extension" - a snap box never does
  (we never install there now), so skipping is correct, but the warning prevents
  the silent-no-op trap.

**The `--policy-file` override must not bypass the snap guard.** Today `run()`
resolves `opts.policy_file` *before* calling `detect_firefox()`
(`install.rs:413`): `--policy-file <PATH>` short-circuits detection and writes
that policy unconditionally. On a snap box that writes a `policies.json` snap
cannot read - the exact silent mis-install this design exists to kill, just
through a different door. So snap detection must run **first**, before the
`--policy-file` branch: if `is_snap_firefox()` is true, snap's `run()` behavior
above applies (hard error on explicit, warn+skip on `--if-installed`)
*regardless of `--policy-file`*. The override remains a real escape hatch for
non-snap installs whose path detection fails (`Unknown`); it just cannot be used
to force a policy onto a sandbox that ignores it.

**Refresh semantics - what a deploy actually updates.** The force-install
`install_url` is the stable `file://.../obsidian-borg-latest.xpi` symlink, not a
versioned name. So on every deploy after the first, the merged `policies.json`
is byte-identical to the existing file → `policy_changed == false` → the policy
file is correctly **not** rewritten. The deploy is still *not* a no-op: because
the policy already contains our entry, `is_already_installed()` returns true, so
`run()` proceeds to re-sign the `.xpi` and atomically swap the
`obsidian-borg-latest.xpi` symlink to the new versioned artifact. **The on-disk
extension is refreshed every deploy; the policy file is stable by design.**
What a deploy does *not* do is hot-reload a *running* Firefox: Firefox evaluates
enterprise policy and force-installed `file://` extensions at startup, not via an
inotify watch on the symlink target. Picking up a freshly-deployed `.xpi`
therefore requires a Firefox restart. This is acceptable for an always-on
personal tool (Firefox restarts routinely), but the design does not claim
live in-place upgrade of a running browser.

### Data Model

`FirefoxInstall` keeps its variants (`Tarball`, `AptOrDeb`, `Snap`, `Flatpak`,
`Unknown`). `InstallStrategy` loses `ProfileExtension`, keeping only
`PolicyFile { path }`. Removing the enum variant is the compiler-enforced way to
guarantee no code still copies into a snap profile.

### API Design

No CLI surface change. `sb borg extension install [--no-policy] [--if-installed]
[--policy-file <PATH>]` is unchanged. Only internal strategy resolution and the
snap branch behavior change. The snap error message (single source):

```
snap Firefox is unsupported - its sandbox cannot load the capture extension
(POSTs to the local daemon silently fail; see
docs/postmortems/2026-06-07-firefox-snap-breaks-borg-capture.md).
Install Mozilla's /opt Firefox and remove the snap:
    manifest -C ~/repos/scottidler/dotfiles/manifest.yml -s firefox-opt | bash
then re-run this command.
```

### Implementation Plan

#### Phase 1: Remove the snap install path, keep snap detection as an error
**Model:** opus
- In `policy_path()`: replace the `Snap` bail text with the canonical migration
  message (extract to a `const SNAP_UNSUPPORTED: &str` or a small helper so
  `install_strategy`, `policy_path`, and `run` share one string).
- In `install_strategy()`: replace the `Snap => ProfileExtension{...}` arm with
  `Snap => eyre::bail!(SNAP_UNSUPPORTED)`.
- Delete `InstallStrategy::ProfileExtension` and `snap_active_profile_dir()`.
- **Extract the pure path classifier.** Pull the `/opt/firefox`, `/usr/bin`,
  flatpak, `/snap/` string matching out of `detect_firefox()` into
  `fn classify_firefox_path(resolved: &str) -> FirefoxInstall`. `detect_firefox()`
  becomes: snap probe → `which_firefox()` → `classify_firefox_path(&resolved_str)`.
  This is what makes the Phase 2 canonicalize-trap test possible.
- In `run()`: **run snap detection before the `--policy-file` branch.** The
  current ordering checks `opts.policy_file` first (`install.rs:413`) and skips
  detection; reorder so a snap box hits the snap behavior even when
  `--policy-file` is passed. When snap is detected, branch on `opts.if_installed`
  - true → `log::warn!` the remedy and return `skipped_not_installed: true`;
  - false → `eyre::bail!(SNAP_UNSUPPORTED)`. Do this *before* both
    `--policy-file` resolution and `install_strategy` so the message is precise,
    `--policy-file` cannot force a policy onto snap, and we never reach a generic
    strategy error.
- Delete the now-unused `ProfileExtension` match arm in `run()` and `uninstall()`.
- Keep `is_snap_firefox()` and the `Snap` variant (still detected).

#### Phase 2: Delete now-dead profile parser + fix tests
**Model:** sonnet
- **Delete `parse_default_profile_path()`** and its tests. Resolved (was Open
  Question 1): its only caller is `snap_active_profile_dir()`, removed in Phase 1;
  the policy-file path never parses a profile. It is `pub` but unused outside snap,
  so it is dead - remove it rather than leave a `#[allow(dead_code)]` (per rust.md).
- Remove the snap-path tests (`ProfileExtension`, `snap_active_profile_dir`,
  `parse_default_profile_path`) from `borg/src/extension/install/tests.rs`.
- Add: `install_strategy(Snap)` returns an error containing "unsupported".
- Add: `classify_firefox_path("/opt/firefox/firefox")` → `Tarball`,
  `classify_firefox_path("/usr/bin/firefox")` → `AptOrDeb`,
  `classify_firefox_path("/snap/firefox/...")` → `Snap`, flatpak markers →
  `Flatpak`, anything else → `Unknown`. This is the regression guard against the
  canonicalize-wrapper trap (Alternative 2) - it tests the pure helper extracted
  in Phase 1, not `detect_firefox()` (which shells out and so is not
  deterministically testable on a host with any given Firefox).
- Add: `run()` with `--policy-file` set on a snap box still errors (or warn+skips
  under `--if-installed`) - guards the override-loophole fix from Phase 1. Drive
  this through the pure snap-branch logic so it does not depend on the host
  actually running snap.
- `cargo test -p borg`; `otto ci`.

#### Phase 3: `sb doctor` snap-Firefox warning
**Model:** sonnet
- Resolved (was Open Question 2): add a `sb doctor` check that WARNs when snap
  Firefox is present ("snap Firefox present - it breaks capture; migrate to /opt
  via `manifest -s firefox-opt`"). Makes the abandoned-snap state observable
  instead of a surprise, and reuses the detection we are keeping. Small, fits the
  existing doctor surface (which already reports signal/daemon state).
- **Cross-crate note:** the doctor checks live in the `sb` crate
  (`sb/src/...`), but `is_snap_firefox()` is **private** to `borg`
  (`install.rs:84`). Do **not** make it `pub` just for this. `detect_firefox()`
  is already `pub` and returns `FirefoxInstall::Snap`, so the doctor check is
  `matches!(borg::extension::install::detect_firefox()?, FirefoxInstall::Snap)`.
  Reuse the public enum-returning API; keep the shell-probe helper private.

#### Phase 4: Bootstrap force-install on desk + lappy
**Model:** sonnet
- On desk: `sudo sb borg extension install` (no flags) → signs the `.xpi` via AMO,
  writes `/opt/firefox/distribution/policies.json` force-installing it. The policy
  entry already sets `default_area: navbar`, so the toolbar icon appears without a
  manual pin. The write is idempotent (`merge_policy` + `policy_changed` guard).
- On lappy: same, after the `firefox-opt` manifest run completes there.
- Verify: launch `/opt` Firefox → extension present in the navbar without a manual
  add; on desk a click yields an `http` receipt; on lappy set the endpoint to
  `http://desk.lan:8181` in the options page (force-install does NOT seed
  `storage.local`, so the default `localhost:8181` is wrong on lappy).

### Acceptance criteria (done when)

- On a **snap** box: `sb borg extension install` exits non-zero with the
  `SNAP_UNSUPPORTED` message; `--if-installed` warns + skips (exit 0), and
  `otto deploy` still succeeds.
- On an **`/opt`** box: `sudo sb borg extension install` writes the policy; `/opt`
  Firefox shows the extension in the navbar with no manual add; a toolbar click
  produces an `http` receipt in the daemon DB.
- On a **daemon-only / headless** box (no Firefox): `--if-installed` skips cleanly
  (the *correct* no-op - distinct from the old snap silent no-op).
- Subsequent `otto deploy` runs re-sign the `.xpi` and swap the
  `obsidian-borg-latest.xpi` symlink; the deploy log shows a refresh, not
  "extension not installed; skipping". The `policies.json` itself is **not**
  rewritten (`policy_changed == false`) because the force-install URL is the
  stable symlink - this is correct, not a failure. A *running* Firefox picks up
  the new `.xpi` only after a restart (enterprise `file://` extensions are
  re-evaluated at startup, not hot-reloaded); the design does not claim live
  in-place upgrade.
- On a **snap** box, `sb borg extension install --policy-file <PATH>` does **not**
  write the policy - snap detection runs before the override and the command
  errors (explicit) or warn+skips (`--if-installed`). The override cannot force a
  policy onto a sandbox that ignores it.
- `cargo build` has **zero** dead-code / unused warnings (proof the snap paths and
  `parse_default_profile_path` are fully excised, not `#[allow]`-silenced).

### Rollout Plan

Ship in the normal `bump && otto deploy` flow. After this lands, the deploy hook
(`--if-installed`) finds the force-install policy on each machine and refreshes
the signed `.xpi` every deploy. First-machine bootstrap stays
`sudo sb borg extension install` (now the only path that writes the policy).

## Alternatives Considered

### Alternative 1: Model B - manual AMO install + AMO auto-update
- **Description:** User installs from AMO; deploy only signs+publishes; AMO pushes
  updates. Remove the policy/`--if-installed` machinery entirely.
- **Pros:** No enterprise-policy footprint; user-removable; matches "I'll add the
  web-ext" expectation.
- **Cons:** Requires a listed AMO listing or a self-hosted `update_url` for
  auto-update; leaves the refresh outside our deploy; more moving parts we don't
  control. Doesn't make `otto deploy` do anything useful.
- **Why not chosen:** User chose A. Force-install makes deploy authoritative and
  removes every manual step.

### Alternative 2: Delete snap detection entirely
- **Description:** Drop `is_snap_firefox()`; rely on path detection only.
- **Pros:** Less code.
- **Cons:** **Silent failure** - snap's `/usr/bin/firefox` wrapper canonicalizes
  to `/usr/bin/...` → misclassified as `AptOrDeb` → writes a policy snap can't
  read → extension silently never loads. Reintroduces the exact class of bug this
  whole effort exists to kill.
- **Why not chosen:** Fails unsafe. Detection must stay; only the install path goes.

### Alternative 3: Keep snap as a working ProfileExtension path
- **Description:** Leave the snap copy-into-profile install working.
- **Pros:** Zero change.
- **Cons:** Maintains a path to a configuration proven broken for capture; a
  footgun if snap ever returns.
- **Why not chosen:** snap is abandoned and apt-pinned; supporting it invites the
  regression back.

## Technical Considerations

### Dependencies
No new crates. `web-ext` (already required by `sign`) + AMO creds
(`MOZILLA_JWT_*`, present in `/run/user/1000/borg.env`) for the bootstrap sign.

### Security
Force-install via enterprise policy means the extension is operator-managed and
not user-removable from `about:addons` - acceptable and intended for a personal
always-on capture tool. The `.xpi` is AMO-signed.

### Testing Strategy
Unit tests in `borg/src/extension/install/tests.rs` (strategy resolution + the
canonicalize-trap guard). Integration: `stage_produces_valid_extension_dir.rs`
and `extension_body_matches_ingest_request.rs` are unaffected. Manual smoke:
bootstrap on desk, click, confirm receipt.

### Rollout Plan
See above - normal `bump && otto deploy`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| snap box misdetected as apt/deb (silent) | Med (if detection removed) | High | Keep `is_snap_firefox()`; snap → hard error; test the apt/deb-vs-opt classification via the pure `classify_firefox_path` helper |
| `--policy-file` bypasses snap guard (silent mis-install) | Low | High | Run snap detection before the `--policy-file` branch in `run()`; the override cannot write a policy snap ignores; covered by a Phase 2 test |
| Operator expects deploy to hot-reload a running Firefox | Med | Low | Document that deploy refreshes the on-disk `.xpi` but a Firefox restart is needed to load it; `policy_changed == false` on re-deploy is expected, not a bug |
| Force-install policy not read by `/opt` Firefox | Low | High | `policies.json` in `<firefox>/distribution/` is the documented Mozilla mechanism; verify on desk before declaring done |
| Bootstrap sign fails (AMO creds absent) | Low | Med | creds in `borg.env`; surface the existing `sign` error; non-fatal in deploy hook |
| lappy endpoint still localhost after force-install | Med | Med | force-install doesn't seed `storage.local`; document setting `desk.lan:8181` in options post-install |

## Open Questions

None outstanding - both prior open questions were resolved in the Excellence pass:
- `parse_default_profile_path()` → **deleted** (dead after Phase 1; Phase 2).
- `sb doctor` snap warning → **added** (Phase 3).

Architect review (Gemini) findings, all folded into the plan above:
- **`--policy-file` loophole** → snap detection now runs before the override in
  `run()`; the override cannot force a policy onto snap (Phase 1, Acceptance, Risks).
- **`sb doctor` cross-crate** → check uses the public `detect_firefox()` enum, not
  the private `is_snap_firefox()` (Phase 3).
- **Deploy hot-reload assumption** → clarified: deploy refreshes the on-disk
  `.xpi`, `policy_changed == false` is expected, a running Firefox needs a restart
  (Architecture, Acceptance, Risks).
- **`detect_firefox()` untestable** → pure `classify_firefox_path()` extracted so
  the canonicalize-trap guard is deterministic (Architecture, Phase 1, Phase 2).

This doc is the code-side completion of the snap abandonment: the postmortem
records *why* snap is gone, the dotfiles `firefox-opt` script installs the *browser*
under `/opt`, and this change makes the *installer* treat `/opt` as the only
supported target and snap as a loud, actionable error. Together they close the
loop so the capture saga cannot silently regress.

## References
- `docs/postmortems/2026-06-07-firefox-snap-breaks-borg-capture.md` - why snap is gone
- `borg/src/extension/install.rs` - the installer being cleaned up
- `dotfiles/manifest.yml` `firefox-opt` - the `/opt` install pattern
- `.otto.yml` `deploy` task - the `--if-installed` extension refresh hook
