# Implementation Notes: Background-Independent Extension Capture (Popup)

Design doc: `docs/design/2026-06-03-extension-popup-capture.md`

## Phase 1+2: Extension assets, manifest, and tests (combined)

### Design decisions
- `borg/clients/extension/popup.html` includes a 48px Locutus image beside the status line, not a text-only page. Added at the operator's request during execution; the `locutus-48.png` asset was already bundled.
- `popup.js::fail()` uses `icons/locutus-48.png` as the desktop-notification icon, matching what the removed `background.js` did, for visual continuity.
- `commands._execute_action` carries only `suggested_key` (no `description`); reserved command names are handled by Firefox and do not take a description.

### Deviations
- **Combined design-doc Phases 1 and 2 into a single feature commit.** Removing `background.js` and changing the manifest breaks the existing `manifest`/`stage`/`body-match` tests until they are updated in the same change, so the two phases cannot pass `otto ci` independently. The skill's one-commit-per-phase guidance assumes phase independence, which does not hold here.
- **Added a Locutus image to `popup.html`.** The design doc's API Design specified a text-only `#status`. The image is an explicit operator request, not a gap-fill.
- **Separate `style: cargo fmt` commit (8338c57) precedes the feature.** `otto ci`'s fmt check failed on pre-existing rustfmt drift in `cortex/src/intel.rs` and `sb/src/cli/cortex.rs` (unrelated to this feature, almost certainly left by the earlier `feat(intel)` commit under a newer rustfmt). Fixed in isolation so the feature commit stays clean; none of the feature's own files needed formatting.

### Tradeoffs
- Kept the `Alt+Shift+B` keyboard shortcut (now via `_execute_action`) rather than dropping it. The doc left this as an operator-preference open question; default is keep. The cost is a focus-steal mid-typing, accepted per Architect consensus as the irreducible price of background-independent keyboard capture.
- Popup shows a ~400ms "Queued" confirmation before self-closing (doc default), rather than instant close on dispatch.

### Open questions
- Operator preference: keep the ~400ms "Queued" confirmation, or close instantly on dispatch?
- Operator preference: keep the `Alt+Shift+B` shortcut (accept focus-steal) or drop it (toolbar click only)?
- Confirm the Locutus image on the popup is wanted long-term, or revert `popup.html` to text-only if the flash feels cluttered.

## Phase 3: Ship

### Design decisions
- The outward-facing portion of Phase 3 (AMO re-sign via `otto deploy`, Firefox restart, and click-to-verify) is performed as an interactive handoff rather than autonomously: `otto deploy` is a multi-minute AMO upload, and the verify step physically requires the operator's Firefox (restart + click). The local pipeline (commits, version bump, tag) is completed autonomously.

### Deviations
- None.

### Tradeoffs
- `bump` (patch) is mandatory before the re-sign: `sign::run` keys its reuse decision on the version string, not on content, so an unchanged version would ship the stale `.xpi`.

### Open questions
- None.
