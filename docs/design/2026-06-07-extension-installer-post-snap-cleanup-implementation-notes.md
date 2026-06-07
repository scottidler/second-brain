# Implementation notes: extension installer post-snap cleanup

Running record of decisions made while executing
`docs/design/2026-06-07-extension-installer-post-snap-cleanup.md`. Append-only.

## Phase 1: Remove the snap install path, keep snap detection as an error

### Design decisions
- Extracted `snap_run_outcome(if_installed: bool) -> Result<InstallResult>`
  (`borg/src/extension/install.rs`) as a pure helper for the snap branch in
  `run()`. The design specified the *behavior* (explicit -> bail, `--if-installed`
  -> warn+skip) and the Phase-2 *test* for it, but not a named helper; factoring
  it out is what makes that test deterministic without mocking `detect_firefox()`
  (which shells out to `snap`/`which`). Fills a gap the doc left implicit.
- `policy_path(Snap)` and `install_strategy(Snap)` both bail with the shared
  `SNAP_UNSUPPORTED` const (`install.rs`), as the design's "one message, one
  place" required.

### Deviations
- None. The snap *detection* (`is_snap_firefox`, `FirefoxInstall::Snap`,
  `classify_firefox_path`'s `/snap/` arm) is retained exactly as the design
  mandated; only the install *path* (`ProfileExtension`, `snap_active_profile_dir`)
  was removed.

### Tradeoffs
- `snap_run_outcome` extraction vs. leaving the snap block inline and relying on
  manual smoke + the `policy_path/install_strategy` error tests. Chose extraction:
  the architect's primary finding was the `--policy-file` loophole, so a
  deterministic unit proof that the snap guard fires (and precedes `--policy-file`)
  is worth the one small private helper.

### Open questions
- None.

## Phase 2: Delete now-dead profile parser + fix tests

### Design decisions
- The `--policy-file`-on-snap guarantee is tested at the `snap_run_outcome` level
  (`install/tests.rs`: `snap_run_outcome_bails_on_explicit_install`,
  `snap_run_outcome_skips_under_if_installed`) rather than by driving the full
  `run()` (which shells out via `detect_firefox`). The ordering guarantee
  itself - that `run()` calls the snap check before the `--policy-file` branch -
  is structural in `run()` and verified by reading, not by a test that mocks the
  shell-outs. This is the faithful reading of the design's "drive this through
  the pure snap-branch logic so it does not depend on the host running snap."

### Deviations
- None. `parse_default_profile_path()` and all five of its tests deleted as
  specified; the three new test groups (snap strategy error, classifier mapping,
  snap run outcome) added.

### Tradeoffs
- `classify_firefox_path` test includes a realistic `/snap/.../firefox` path that
  also exercises the `/snap/` belt-and-suspenders arm, not just the happy
  `/opt` / `/usr/bin` cases - the canonicalize-trap is the whole reason the
  helper exists.

### Open questions
- None.

## Phase 3: `sb doctor` snap-Firefox warning

### Design decisions
- Added a pure `firefox_finding(&FirefoxInstall) -> Finding` mapper alongside the
  `firefox_findings()` section fn (`sb/src/cli/checks.rs`), so the snap-warns /
  others-ok decision is unit-testable without the host running any particular
  Firefox - mirrors the Phase-1 `classify_firefox_path` split. Design called for
  the check and the public-API reuse; the pure mapper is the gap-fill that makes
  it testable.
- The section reports **all** install types, not only snap: Ok for capture-capable
  (`/opt` tarball, apt/deb, flatpak), Warn for snap, Info for "not detected." The
  design's Phase 3 text says "WARN when snap is present"; emitting a positive Ok
  for the healthy case matches every other doctor section (none stay silent on
  success) and gives the operator confirmation the capture browser is the right
  one. Slight scope addition, consistent with the surrounding code.
- Used the public `borg::extension::install::detect_firefox()` (enum-returning),
  not the private `is_snap_firefox()` - exactly as the architect-revised design
  required; no new public surface added to borg.

### Deviations
- None.

### Tradeoffs
- Test placement: added to the existing inline `#[cfg(test)] mod tests` block in
  `checks.rs` to match the file's established pattern, rather than extracting to a
  `checks/tests.rs` submodule (rust.md's preferred layout). Extraction of this
  pre-existing inline block is a separate tree-wide refactor, deliberately not
  mixed into this feature.

### Open questions
- None.

## Phase 4: Bootstrap force-install on desk + lappy (OPERATIONAL - not code)

### Design decisions
- This phase is operator action, not code: `sudo sb borg extension install` on
  desk (and on lappy after its `firefox-opt` manifest run), which signs the .xpi
  via AMO and writes `/opt/firefox/distribution/policies.json`. It requires sudo,
  AMO creds (`MOZILLA_JWT_*` in `/run/user/1000/borg.env`), and the physical
  machines. It cannot run in CI and is therefore NOT executed by this plan.

### Deviations
- Not performed here. The code that Phase 4 exercises is complete and shipped in
  Phases 1-3; the bootstrap itself remains a manual rollout step for the operator.

### Tradeoffs
- Marking the design "Implemented" on code-completion (Phases 1-3) while Phase 4
  remains a manual rollout. The alternative - withholding "Implemented" until the
  browsers are bootstrapped - would conflate code state with deployment state.

### Open questions
- Operator: run `sudo sb borg extension install` on desk, then on lappy (after
  `firefox-opt`), and set the lappy options endpoint to `http://desk.lan:8181`
  (force-install does not seed `storage.local`). Verify a toolbar click yields an
  `http` receipt on desk.

## Post-Architect-review refinement (supersedes Phase 1 snap-guard mechanism)

### Design decisions
- The snap guard in `run()` now calls `is_snap_firefox()` (returns `bool`, never
  errors) instead of `detect_firefox()?`. Functionally identical for the loophole
  fix, but strictly cleaner and it preserves the `--policy-file`
  "managed-environment escape hatch": the previous `detect_firefox()?` at the top
  of `run()` shelled out to `which firefox` unconditionally and would propagate a
  spawn error, breaking `--policy-file` precisely in the unreliable-detection
  environment it exists for. `detect_firefox()` is now back to being called only
  in the non-`--policy-file` branch, as it was before this change. Caught in the
  advisor review of the shipped code.

### Deviations
- None from the design (the doc says "run snap detection before the
  `--policy-file` branch" - still exactly what happens; only the primitive
  changed from `detect_firefox()?` to `is_snap_firefox()`).

### Tradeoffs
- `is_snap_firefox()` vs `detect_firefox()?`: the former loses the `/snap/`-path
  belt-and-suspenders arm at the `run()` guard, but that arm only matters when
  `snap list firefox` fails to spawn while Firefox resolves under `/snap/` -
  impossible in practice (a `/snap/` Firefox implies snap is installed). The
  non-`--policy-file` branch still routes such a path to `install_strategy(Snap)`
  -> bail, so it remains safe.

### Open questions
- None.
