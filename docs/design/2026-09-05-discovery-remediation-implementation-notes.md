# Implementation Notes: Discovery Remediation

Design doc: `docs/design/2026-09-05-discovery-remediation.md`

## Phase 0: Ship the in-flight --maxTokens work (R7)

### Design decisions
- Committed the design doc first, standalone, before touching any tracked
  file — `docs(design): discovery remediation` (`3c6de89`) — per the doc's
  explicit ordering instruction.
- Folded two clippy fixes into the Phase 0 commit rather than a separate one:
  `chunks_exact` -> `as_chunks::<4>()` in `vault/src/search/vector.rs:162,325,388`,
  and `#[allow(clippy::result_unit_err)]` on `borg::notify::Telegram::processing`
  and `borg::notify::Signal::processing` (`borg/src/notify.rs:114,432`). Both
  lints are newly enforced by the local `rustc`/`clippy` 1.98.0 toolchain
  (`.github/workflows/release.yml` pins CI to 1.96.0) and were failing `otto
  ci` on files untouched by the 9-file `--maxTokens` diff. Fixing them at the
  correct minimal seam (mechanical rewrite; `#[allow]` with a rationale
  comment rather than redesigning the public `Result<(), ()>` API) was
  required to get `otto ci` green at all, so it rode in this commit instead of
  blocking the phase.
- Appended the exact paragraph specified in the doc's Phase 0 bullet to
  `docs/design/2026-08-30-video-distill-token-budget.md` Resolved Decisions,
  verbatim, and touched nothing else in that file.

### Deviations
- **Ordered deferral, not a spec gap:** `bump && otto deploy` was explicitly
  withheld per the team lead's instruction. Those are held for a finalization
  checkpoint after all 16 phases land, not run per-phase. Consequently the
  third success criterion (`sb doctor | grep -c 'maxTokens'` >= 1 on the
  *deployed* binary) is DEFERRED-TO-DEPLOY: it cannot be true until that
  finalization deploy happens.
- Two files outside the doc's named 9 (`borg/src/notify.rs`,
  `vault/src/search/vector.rs`) are in this commit. Both changes are
  toolchain-drift clippy fixes, not behavior changes, and were necessary for
  `otto ci` to pass under the locally installed 1.98.0 toolchain regardless
  of this phase's diff (verified: the two lints fire on baseline `f97718f`
  code untouched by the 9-file diff). No other phase in this doc claims these
  two files.

### Tradeoffs
- `#[allow(clippy::result_unit_err)]` vs. introducing a real error enum for
  `notify::Telegram`/`notify::Signal::processing`: chose the allow. A typed
  error would ripple into every caller of both `processing` fns across
  `borg/src/pipeline*`, which is out of Phase 0's scope and belongs to
  whichever phase (if any) later touches notify's error contract.

### Open questions
- None.
