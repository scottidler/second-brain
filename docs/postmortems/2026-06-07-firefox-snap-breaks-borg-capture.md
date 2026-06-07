# Postmortem: snap Firefox silently breaks the obsidian-borg capture extension

**Date:** 2026-06-07
**Severity:** capture from the browser button produced zero ingestion; daemon healthy the whole time
**Root cause:** Ubuntu's **snap** packaging of Firefox, not any code in this repo

## One-line summary

The Firefox "capture" web-extension's `POST` to the local borg daemon never lands
when Firefox is the **snap** build, even though the daemon, the network path, and
the extension code are all correct. The identical extension works in a non-snap
(`/opt`) Mozilla Firefox. Fix: replace snap Firefox with Mozilla's official build
under `/opt` (codified as the `firefox-opt` script in `dotfiles/manifest.yml`).

## Why this took so long (the misdiagnosis chain)

This bug "broke and worked" across many sessions because each session fixed a
*symptom* and never reached the environment. In order:

1. **`fa79724` (May 12) - the original false break.** "fix(borg): fire-and-forget
   HTTP /ingest" changed the `/ingest` response from an awaited terminal
   `IngestResult {title, status}` to an immediate `{status:"Queued", title:null}`.
   The extension's old `background.js` checked `result.title`/`result.status.Failed`,
   so after this it **silently stopped showing success/failure feedback**. Telegram
   kept working because the detached `tokio::spawn` task still calls
   `notifier.result(...)`. The extension still *ingested* - only the feedback died.
   This was admitted in session `124a2921` but mis-framed as the cause of "nothing
   happens."

2. **`9375b29` (Jun 3) - the wrong fix.** On the theory that the toolbar
   `chrome.action.onClicked` had stopped firing ("MV3 background death" - a theory
   the user explicitly rejected, since it had worked for weeks), the architecture
   was swapped to `action.default_popup` + `popup.js`. This was a fix built on a
   misdiagnosis and introduced a *new*, real failure surface.

3. **`1c3deb0` -> `4556577` - the keepalive flip-flop.** `1c3deb0` empirically found
   `keepalive: true` on the popup's `fetch` broke capture on snap Firefox and
   removed it. `4556577` "restored it per spec" calling `1c3deb0` a "spurious
   correlation." This is the literal "breaking and working back and forth" in the
   git log. (keepalive removal was kept this round, but it was **not** the root
   cause - see below.)

Every step debugged the browser/extension/daemon. None reached the snap boundary.

## What was actually proven (2026-06-07)

Hard evidence gathered by querying the daemon's receipts DB
(`~/.local/share/sb/borg/receipts.db`; the door writes a `received` row
synchronously before any processing, so "did the POST arrive" == "did a row
appear"):

- **Daemon is perfect.** `curl -XPOST localhost:8181/ingest -d '{"url":...}'` ->
  HTTP 200, receipt written (`ht-2797e6`). `GET /health` -> 200. CORS is
  `allow_origin(Any)`, methods `GET,POST`, headers `content-type` (`borg/src/lib.rs:79`).
- **snap Firefox can reach the daemon at the chrome level.** Typing
  `http://localhost:8181/health` in the snap Firefox address bar returns the
  JSON. So it is **not** a network/confinement-socket block.
- **The extension code is correct.** Driven under Playwright **Chromium**, a fetch
  from inside the real `chrome-extension://.../popup.html` context (same CSP
  `connect-src` + host permissions) reached the daemon and wrote a receipt
  (`ht-32f506`).
- **The extension works in non-snap Firefox.** A throwaway background-script build
  driven by `web-ext run` against a **/opt Mozilla Firefox 151** wrote markers
  `OPT-1-bg-loaded` and `OPT-3-taburl-https://www.youtube.com/...` - i.e. the
  extension ran, read the active tab URL, and the POST landed. Identical extension,
  identical daemon.
- **snap Firefox cannot even be automated.** `web-ext run` against the snap
  Firefox failed with `ECONNREFUSED` connecting to its debugger - snap confinement
  blocks the tooling outright.
- **In the user's snap Firefox, the extension POST never lands** (zero receipts
  across many clicks, with and without keepalive, popup rendering blank/white).

Conclusion: the only variable that flips the outcome is **snap vs non-snap
Firefox**. snap Firefox's confinement breaks the extension's capture path while
leaving address-bar networking intact. This is consistent with snap Firefox's
well-known breakage of extension capabilities (native messaging, localhost from
extension contexts, automation).

## The fix

1. **Replace snap Firefox with Mozilla's official build under `/opt`** on every
   machine. Codified as the `firefox-opt` entry in
   `~/repos/scottidler/dotfiles/manifest.yml` (`manifest -s firefox-opt | bash`):
   - download `firefox-latest` linux64 tarball to `/opt/firefox`
   - symlink `/opt/firefox/firefox` -> `/usr/local/bin/firefox` (ahead of the snap shim)
   - install a `/usr/share/applications/firefox.desktop` pointing at `/opt`
   - `snap remove firefox` (snapd auto-saves a recoverable data snapshot)
   - apt-pin `/etc/apt/preferences.d/no-firefox-snap` so the Ubuntu transitional
     `firefox` deb (`1:1snap1-*`, which reinstalls the snap) can never restore it
   - Bookmarks/logins/most add-ons return via Firefox Sync; **the obsidian-borg
     web-ext must be re-added by hand** afterward.
   Applied to **desk** and **lappy** (`ltl-7007`) on 2026-06-07.

2. **Extension hardening kept this round** (defense in depth, not the root cause):
   - `keepalive: true` removed from `popup.js` (1c3deb0's empirical finding; the
     fire-and-forget daemon returns in ~17ms so it is unnecessary) with a CI
     regression guard (`borg/tests/stage_produces_valid_extension_dir.rs`:
     `popup_js_must_not_use_fetch_keepalive`) so it can never be "restored per spec"
     again.
   - inline `<style>` extracted to `popup.css`/`options.css` (the manifest CSP
     `default-src 'self'` blocked inline styles -> the blank/unstyled popup);
     added to the staged-asset list in `borg/src/extension.rs`.

## Per-machine endpoint note

The capture web-ext reads its endpoint from `chrome.storage.local["endpoint"]`,
defaulting to `http://localhost:8181`. After re-adding the extension:

- **desk** (runs the daemon): default `http://localhost:8181` is correct.
- **lappy** (`ltl-7007`, POSTs over LAN): set the endpoint to `http://desk.lan:8181`
  via the extension's options page. `desk.lan` is covered by the extension's
  `*.lan` host permission and CSP `connect-src`.

## If snap firefox ever needs to come back

`snap saved` holds the auto-snapshot taken at removal; `snap restore <id>` brings
the old profile back. But don't - it's the bug.
