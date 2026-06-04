# Design Document: Background-Independent Extension Capture (Popup)

**Author:** Scott Idler
**Date:** 2026-06-03
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The Firefox capture extension stops working when its MV3 background context dies mid-session, and only a Firefox restart recovers it. This redesigns the toolbar action to fire from a short-lived popup page instead of a long-lived background listener, so a single click captures the current tab even when no background context is alive. The capture path stops depending on a process that can crash.

## Problem Statement

### Background

`obsidian-borg Capture` sends the active tab's URL to the borg daemon (`POST http://localhost:8181/ingest`). Today the toolbar button works like this:

1. Click fires `chrome.action.onClicked` (manifest has no `default_popup`).
2. Firefox wakes the **background** context (`borg/clients/extension/background.js`).
3. `captureTab()` reads the active tab, POSTs `{url}`, sets a badge, and relies on the daemon's desktop notification sink for the Saved/Duplicate/Failed outcome.

The background is declared with both keys:

```json
"background": { "scripts": ["background.js"], "service_worker": "background.js" }
```

### Problem

On 2026-06-03 the button silently stopped delivering. The daemon was healthy and the same endpoint worked under `curl`; the receipts DB and `system/intake/` showed zero arrivals for two days. The extension's host permission for `http://localhost` was granted, the endpoint was correct, and no extension or daemon files changed in the failure window. A reinstall plus Firefox restart restored it.

The operator's history rules out routine MV3 idle-termination: receipts `ht-176370` and `ht-af250c` landed 3 seconds apart (two consecutive clicks, both succeeded), and the extension ran flawlessly for weeks across many sessions. If the background needed re-warming on every idle gap, consecutive clicks would fail; they did not. The accurate diagnosis is that the background context **died once, mid-session, and Firefox never re-spawned it** for the rest of a weeks-long session. The exact trigger is unrecoverable (Firefox restarted, clearing the background's error log).

The structural issue: any silent toolbar-to-badge design must route the click through the background, and the background can die irrecoverably within a long-lived session. For an operator who keeps Firefox running for weeks, one background death equals weeks of silent capture loss.

### Goals

- A single toolbar click (and the keyboard shortcut) captures the active tab even when no background context is alive.
- No dependency on Firefox waking or re-spawning a long-lived background context for the capture path.
- Preserve the current operator experience as closely as possible: one click, fire-and-forget, outcome delivered by the daemon's existing desktop notification.
- Keep the build pipeline (`stage` + `build_manifest`) and its test coverage authoritative.

### Non-Goals

- Fixing the root cause of the Firefox background crash. That is a browser-side bug we cannot control.
- A heartbeat / liveness-detection feature (separate design; bigger change; still requires a manual restart and does not prevent loss).
- The `sb borg log --since` filter bug (separate, unrelated fix).
- Any change to the daemon's `/ingest` contract or the `IngestRequest` shape.

## Proposed Solution

### Overview

Point the toolbar action at a `default_popup`. Clicking the button opens a fresh popup page on every click; the popup's script runs automatically on load, captures the active tab, POSTs to the daemon, and closes itself. A popup is created new per click, so there is no persistent context to crash or go dormant. Remove the background context entirely, which also eliminates the ambiguous dual `scripts` + `service_worker` declaration.

### Architecture

Components after the change (all materialised by `extension::stage`):

- `popup.html` (new): minimal page that loads `popup.js`; shows a one-line transient status.
- `popup.js` (new): on `DOMContentLoaded`, resolve endpoint from `chrome.storage.local` (default `http://localhost:8181`), query the active tab, `POST {url}` to `/ingest`, await the `Queued` response, then `window.close()`. On a privileged/empty tab URL or a fetch failure, show the reason and skip auto-close.
- `options.html` / `options.js` (unchanged): endpoint configuration.
- `background.js` (removed): no long-lived context remains.

Manifest changes (`borg/src/extension/manifest.rs::build_manifest`):

- `action`: add `"default_popup": "popup.html"` (keep `default_icon`).
- `commands`: replace the custom `capture-url` command with the reserved `_execute_action` command, keeping `Alt+Shift+B`. Firefox opens the popup directly for `_execute_action`, with no background involvement.
- `background`: removed entirely (no `scripts`, no `service_worker`).
- `permissions`: keep `activeTab`, `storage`, `notifications`. `activeTab` is granted on the user gesture that opens the popup, so `chrome.tabs.query` can read the active tab URL.
- `content_security_policy.extension_pages`: unchanged; its `connect-src http://localhost:* ...` already authorises the popup's fetch.

Click flow:

```
toolbar click / Alt+Shift+B
  -> Firefox opens popup.html (fresh page, no background needed)
  -> popup.js: getEndpoint() -> query active tab -> POST {url} -> await "Queued"
  -> window.close()
  -> daemon desktop sink delivers Saved/Duplicate/Failed (unchanged)
```

### Data Model

No new persistent data. The POST body is unchanged: `{ "url": <active tab url> }`, which continues to deserialize into `borg::types::IngestRequest`.

### API Design

No daemon API change. The popup calls the existing `POST /ingest` and reads the existing `{status: "Queued", trace_id, ...}` response shape.

Three correctness requirements (raised in Architect review, 2026-06-03) are baked into the snippet below:

1. **`keepalive: true` on the fetch.** A popup is destroyed the instant it loses focus (e.g. the operator clicks back into the page right after triggering capture). `keepalive` instructs the browser to complete the request even if the page unloads, so a focus-loss close does not abort an in-flight POST. Awaiting the response before `window.close()` only covers the programmed close, not the focus-loss close; `keepalive` is the actual guarantee.
2. **Check `res.ok`.** `fetch` does not reject on HTTP 4xx/5xx; it only rejects on network-level failure. Without an `res.ok` check, a daemon-side 500 with a JSON body would parse cleanly and be reported as "Queued". Both failure paths (network reject and non-ok status) route through a single `fail()` handler.
3. **No scheme filter.** Today's `background.js` guards only on `!tab.url` and forwards any scheme (including `file://`) to the daemon. The popup mirrors that exactly; it must not introduce an `https?:` regex, which would be an unacknowledged feature regression (silently dropping `file://` ingestion).

Because the popup can vanish before the operator reads inline text, `fail()` also fires a desktop notification - the durable error channel (this resolves Open Question 2 in favor of keeping the `notifications` permission).

`popup.js` shape:

```js
async function getEndpoint() {
  const data = await chrome.storage.local.get("endpoint");
  return data.endpoint || "http://localhost:8181";
}

function fail(status, message) {
  // The popup can be destroyed on focus loss before the operator reads inline
  // text, so a desktop notification is the durable error channel.
  status.textContent = message;
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icons/locutus-48.png",
    title: "obsidian-borg",
    message,
  });
}

async function capture() {
  const status = document.getElementById("status");
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.url) {                  // mirror current background.js guard; do NOT filter by scheme
    status.textContent = "No active tab URL";
    return;
  }
  const endpoint = await getEndpoint();
  try {
    const res = await fetch(`${endpoint}/ingest`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: tab.url }),
      keepalive: true,                     // finish the POST even if the popup closes on focus loss
    });
    if (!res.ok) {                         // fetch does not reject on 4xx/5xx
      fail(status, `Daemon error: HTTP ${res.status}`);
      return;
    }
    await res.json();
    status.textContent = "Queued";
    setTimeout(() => window.close(), 400);
  } catch (err) {
    fail(status, `Error: ${err.message}`);  // network-level failure
  }
}

document.addEventListener("DOMContentLoaded", capture);
```

### Implementation Plan

#### Phase 1: Extension assets and manifest
**Model:** sonnet
- Add `borg/clients/extension/popup.html`: a small page styled like `options.html`, containing exactly the `<div id="status">` element that `popup.js` writes to, and a `<script src="popup.js">` tag.
- Add `borg/clients/extension/popup.js` (logic in the API Design snippet: `keepalive: true`, `res.ok` check, `!tab.url`-only guard, `fail()` desktop notification).
- Remove `borg/clients/extension/background.js` via `rkvr rmrf`. All three listeners are intentionally retired (Architect-concurred, 2026-06-03): `onClicked` -> `default_popup`; `onCommand` -> reserved `_execute_action` (handled by Firefox, no JS listener); `onInstalled` auto-discovery -> dropped, with first-use failures surfaced by `fail()`'s desktop notification.
- In `borg/src/extension.rs`, update the `static_files` array: drop `background.js`, add `popup.html` and `popup.js`.
- In `borg/src/extension/manifest.rs::build_manifest`: add `action.default_popup`, swap `commands.capture-url` for `commands._execute_action` (keep `Alt+Shift+B`), and remove the `background` object.

#### Phase 2: Tests
**Model:** sonnet
- `borg/src/extension/manifest/tests.rs`:
  - In `manifest_contains_required_top_level_keys`, remove `"background"` from the required-keys list.
  - Add an assertion that `action.default_popup == "popup.html"`.
  - Replace `capture_url_suggested_key_is_alt_shift_b` with `_execute_action` keyed to `Alt+Shift+B`.
- `borg/tests/stage_produces_valid_extension_dir.rs`: update the asset list (remove `background.js`, add `popup.html`, `popup.js`).
- `borg/tests/extension_body_matches_ingest_request.rs`: update the failure message to reference `popup.js` instead of `background.js` (the body assertion itself is unchanged).
- `otto ci` green.

#### Phase 3: Ship
**Model:** sonnet
- `bump` (patch). `sign::run` keys its reuse decision on the version string, not on content (observed: "reusing existing signed .xpi for v0.8.43 ... skipping AMO upload"), so without a bump the changed assets would never reach a new `.xpi`. The bump is mandatory for this change to ship.
- `otto deploy` (re-signs via AMO, refreshes the snap profile copy).
- One final Firefox restart to load the new `.xpi` (snap Firefox does not hot-reload).
- Verify: click the button, confirm a fresh receipt and `system/intake/<trace>.txt` appear.

## Alternatives Considered

### Alternative 1: Event-page-only manifest (keep silent badge)
- **Description:** Remove `service_worker`, keep `scripts: ["background.js"]`, on the theory that the event-page path wakes more reliably than the service-worker path.
- **Pros:** No UX change; smallest possible diff.
- **Cons:** Does not address the observed failure. A silent toolbar-to-badge click inherently requires a live background, and we observed the background can die irrecoverably; event-page vs service-worker does not stop a context from crashing. Mozilla docs also indicate Firefox already ignores `service_worker` when `scripts` is present, so the change may be a no-op.
- **Why not chosen:** Leaves the operator in the exact exposure they reported.

### Alternative 2: Heartbeat / liveness detection
- **Description:** Background pings the daemon on a `chrome.alarms` cadence; the daemon alerts (desktop notification + `sb doctor`) when pings go stale.
- **Pros:** Keeps the silent badge UX; turns "weeks of silent loss" into "a toast within minutes."
- **Cons:** Larger change (new manifest permission, new daemon endpoint, last-seen store, `sb doctor` wiring, notify wiring); still requires a manual Firefox restart; does not prevent capture loss; false-negative if a crash leaves alarms firing but the action dead.
- **Why not chosen:** Detection, not prevention. The operator asked to stop facing this, not to be notified about it. Could be a complementary follow-up.

### Alternative 3: Popup plus a minimal background (onInstalled only)
- **Description:** Keep a tiny background solely for the `onInstalled` endpoint auto-discovery probe; move capture into the popup.
- **Pros:** Preserves auto-discovery of the endpoint on fresh install.
- **Cons:** Re-introduces a long-lived context (and the dual-key question) for a non-critical convenience. The popup already defaults to `http://localhost:8181`, which is correct for the force-installed deployment.
- **Why not chosen:** Removing the background entirely is strictly more robust and removes the manifest ambiguity. Auto-discovery is a minor convenience the default covers.

## Technical Considerations

### Dependencies

No new crates. No new daemon code. Pure extension-asset and manifest-builder changes plus test updates, all inside `borg`.

### Performance

Popup open and self-close is sub-second; the daemon returns `Queued` in milliseconds. No measurable cost.

### Security

No new permissions. The `notifications` permission is **retained as the durable error channel**: the popup can be destroyed on focus loss before the operator reads inline `#status` text, so `popup.js` fires a desktop notification on any failure (network reject or non-ok HTTP status). This resolves the former Open Question about dropping `notifications`. CSP and `host_permissions` are unchanged, so the popup's network reach is identical to today's background. Removing the background reduces, not expands, the extension's resident surface.

### Edge Cases

- **Privileged tab** (`about:`, `view-source:`, `moz-extension:`): `tab.url` is absent or unreadable, so the `!tab.url` guard shows "No active tab URL" and never POSTs. The popup does **not** filter by scheme: `file://` and other non-`http(s)` URLs are forwarded to the daemon exactly as `background.js` does today (no behavior change).
- **Daemon unreachable or erroring:** a network reject or a non-ok HTTP status both route through `fail()`, which shows the reason inline **and** fires a desktop notification. The notification is the channel that survives the popup being closed before the operator reads it.
- **Focus-loss close mid-request:** the operator triggers capture and immediately clicks back into the page, destroying the popup. `keepalive: true` on the fetch guarantees the POST still completes; the request is not aborted at the TCP level.
- **Rapid re-click:** clicking the action while its popup is open toggles it closed (standard Firefox behavior); each completed open captures once. No worse than today, where consecutive clicks already double-fired.
- **`activeTab` via the keyboard command:** invoking the action through `_execute_action` is a user gesture that grants `activeTab`, so `chrome.tabs.query` can read the URL. If a future Firefox ever withholds `activeTab` on the keyboard path, the fallback is to add the broader `tabs` permission; not needed today.

### Testing Strategy

- Unit: `build_manifest` asserts `action.default_popup`, the `_execute_action` key, and the absence of `background`.
- Integration: `stage` materialises `popup.html` / `popup.js` and no longer `background.js`; the POST body still matches `IngestRequest`.
- Manual smoke after deploy: click and the keyboard shortcut both produce a fresh receipt and intake file; a privileged tab (e.g. `about:addons`) shows the "no capturable URL" message and does not POST.

### Rollout Plan

Single coordinated change: code plus tests in one commit, `bump`, `otto deploy` (AMO re-sign), one Firefox restart. No phased rollout; the extension is single-user and self-distributed.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Popup window flash is intrusive | Med | Low | Tiny page; self-closes in ~400ms; documented as the accepted cost of background independence |
| Popup closes (focus loss) before the POST completes, aborting it | Med | Med | `keepalive: true` on the fetch completes the request through page unload; awaiting `Queued` only covers the programmed close |
| Daemon returns 4xx/5xx with a JSON body, masquerading as success | Low | Med | Explicit `res.ok` check; failure routes through `fail()` (inline + desktop notification) |
| Keyboard `_execute_action` steals focus mid-typing | Med | Med | Accepted tradeoff (Architect-concurred): reliable keyboard capture without a popup requires a live background (the thing that died) or `<all_urls>` content-script injection (rejected). Operator may instead drop the shortcut if unused |
| AMO re-sign required for a content change | High | Low | Expected one-time cost; `bump` then `otto deploy` is the standard path |
| Loss of `onInstalled` endpoint auto-discovery | Low | Low | `popup.js` defaults to `http://localhost:8181`; options page still configures it |

## Open Questions

**Resolved (Architect review, 2026-06-03):**
- [x] Keep the `notifications` permission. The popup can be destroyed on focus loss before inline text is read, so a desktop notification is the only durable error channel. Decided: keep, and fire it on every failure.
- [x] Fetch correctness: use `keepalive: true`, check `res.ok`, and do not filter tab URLs by scheme. Baked into the `popup.js` snippet.

**Resolved (Architect consensus, 2026-06-03 round 2):**
- [x] **Keyboard focus-steal fork.** Concur on option (a): accept focus-steal as the irreducible cost of background-independent keyboard capture. There is no Firefox mechanism for reliable silent keyboard capture without a live background or `<all_urls>` content-script injection (rejected as a security regression). `_execute_action` is a net reliability gain since the old silent shortcut was equally dead during the outage. Operator may still elect to drop the shortcut if `Alt+Shift+B` is never used mid-typing.
- [x] **`onInstalled` onboarding replacement.** Concur: drop the install-time `/health` probe and the entire background. A time-of-use desktop notification from `fail()` is a sufficient substitute for this single-user, default-port deployment; re-adding a background solely for install-time probing is bloat that defeats the redesign.

**Still open (operator preference, not Architect):**
- [ ] Popup feedback: instant close on dispatch versus a ~400ms "Queued" confirmation. Doc defaults to the brief confirmation.
- [ ] Whether to keep the `Alt+Shift+B` keyboard shortcut at all, given it now causes a focus-steal. Keep (accept focus-steal) or drop (toolbar click only)?

## References

- `borg/src/extension.rs` (`stage`, `static_files`)
- `borg/src/extension/manifest.rs` (`build_manifest`)
- `borg/clients/extension/background.js`, `options.html`, `options.js`
- `borg/src/extension/manifest/tests.rs`, `borg/tests/stage_produces_valid_extension_dir.rs`, `borg/tests/extension_body_matches_ingest_request.rs`
- `docs/design/2026-05-22-extension-manifest-binary-versioned.md` (current source of truth for extension lifecycle)
- CLAUDE.md: "Firefox extension lifecycle" and the `IngestRequest` additive-evolution rule
