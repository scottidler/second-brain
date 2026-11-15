# Design Document: Extension Endpoint Fallback (Tailscale First, LAN Second)

**Author:** Scott Idler
**Date:** 2026-11-14
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The Firefox capture extension POSTs to one daemon address. Away from home that address is unreachable and the capture fails. This doc makes the extension try an ordered list of addresses: the Tailscale MagicDNS name for ripr first, the LAN name second. The list comes from `borg.yml` on the one host that signs the extension (lappy), is baked into the extension at stage time, and replaces the hand-typed options-page endpoint. A config change without a version bump is a loud error, not a silently reused `.xpi`.

## Problem Statement

### Background

- Borg ingests over HTTP (`POST /ingest`, `borg/src/routes.rs` `ingest`), Telegram, Signal, ntfy (built, not configured on ripr) and Discord (off).
- The extension is the HTTP client. `borg/clients/extension/popup.js` `capture()` reads ONE `endpoint` string from `chrome.storage.local`, defaults it to `http://localhost:8181` (hard-coded in both `popup.js` and `options.js`), and POSTs once. No timeout, no retry.
- A fresh install never learns the daemon address. The force-install policy (`borg/src/extension/install.rs` `build_policy_entry`) carries install fields only, so every non-daemon host types the endpoint into the options page by hand (`docs/design/2026-06-07-extension-installer-post-snap-cleanup.md`).
- ripr, lappy (ltl-7007) and the Pixel are on one tailnet, MagicDNS on, suffix `example.ts.net`. The daemon binds `0.0.0.0:8181`.
- Measured 2026-11-14:
  - `curl http://ripr.example.ts.net:8181/health` -> `200` from ripr and from lappy
  - `ts.net` and `example.ts.net` are not HSTS-preloaded (hstspreload.org status `unknown`), so `http://` is not force-upgraded
  - public DNS (`dig @1.1.1.1`) returns `NXDOMAIN` for `ripr.example.ts.net`; only MagicDNS resolves it
  - an unknown MagicDNS name fails in 0.158s (`Could not resolve host`)
  - a dead tailnet IP (`100.64.0.99:8181`) hangs: curl gave up at its 60s cap. Fallback without a timeout is useless.
  - lappy's Firefox profile carries DoH rollout prefs (`doh-rollout.uri` = Cloudflare, `doh-rollout.home-region` = US)

### Problem

Off the home network the extension's one address (`ripr.lan` / `desk.lan`) does not resolve, so captures from lappy fail. Scott's ask (this session): "tailscale to be the primary. if it fails, http can be a fallback." Both routes are plain HTTP to the same daemon; the fallback is the LAN address.

### Goals

- The extension tries `http://ripr.example.ts.net:8181` first and `http://ripr.lan:8181` second, per capture.
- Fallback fires only when the request never got an HTTP response (DNS failure, connect failure, timeout).
- The address list is config (`borg.yml` on the signing host), not hand-typed in the options page.
- A signed `.xpi` never silently carries a stale config: the same version with different baked config exits 3 and fails `otto deploy`.
- Each attempt has a bounded timeout.
- The popup says which address took the capture; a total failure names every address and its error.

### Non-Goals

- **CLI and hotkey fallback** (`hotkey.host`/`hotkey.port` in `vault::daemon::HotkeyConfig`, used by `sb borg ingest|queue|wait`, `replay`, `migrate`, oracle's `ingest_queue`/`channel_search`, and `borg/clients/hotkey/obsidian-borg-capture.sh`). Excluded: Scott asked about the extension. Parked: revisit if lappy's CLI captures fail away from home.
- **Bookmarklet** (`borg/clients/bookmarklet/README.md`, hard-coded `localhost:8181`). Excluded, same reason.
- **Telegram or ntfy as a fallback route.** Rejected, see Alternatives.
- **Surfacing a 200 response whose body is `Failed`.** The daemon answers some intake failures with HTTP 200 + `IngestStatus::Failed` (`borg/src/routes.rs` `ingest`), and the popup still shows "Queued". Pre-existing, independent of routing. Parked: revisit as its own fix.
- **Daemon auth.** `borg.yml` sets no `server.auth-token`. The tailnet limits who can reach `:8181`. Unchanged here.
- **Remembering the last good route.** Every capture starts at the top of the list. On the LAN with Tailscale up, tailnet traffic goes direct, so the first entry is never the slow one.

## Proposed Solution

### Overview

1. `borg.yml` gains `extension.endpoints` (ordered list) and `extension.attempt-timeout-ms`.
2. The manifest generator adds every endpoint hostname to `host_permissions` and the CSP, so a configured endpoint is always permitted.
3. `sb borg extension stage` writes a generated `daemon.js` carrying the list and timeout.
4. A new `fallback.js` holds one function, `postWithFallback`, that walks the list. `popup.js` calls it.
5. The options page keeps the auth token and shows the endpoint list read-only. The legacy `endpoint` storage key is deleted.
6. `sb borg extension sign` writes a config-hash sidecar beside each signed `.xpi`; reusing an `.xpi` whose hash differs from the current config is an error.

### Architecture

```
borg.yml (signing host: lappy)
  extension.endpoints, extension.attempt-timeout-ms
        |
        v
sb borg extension stage/sign/install  (+ .config-hash sidecar on sign)
  manifest.json  <- origin_patterns(config) now includes endpoint hostnames
  daemon.js      <- generated: endpoints + timeout
  fallback.js, popup.js, options.js  <- static, copied
        |
        v
popup: postWithFallback(endpoints) -> first HTTP response wins
```

- Same seam as `host_permissions`: config -> `stage()` -> generated file. Nothing generated is committed (`docs/design/2026-05-22-extension-manifest-binary-versioned.md`).
- `daemon.js`, not `daemon.json`: the CSP is `default-src 'self'; connect-src http://...`, so `connect-src` does not allow fetching the extension's own files. A `<script src="daemon.js">` falls under `default-src 'self'` and needs no CSP change.
- No background script, no `keepalive` (`docs/design/2026-06-03-extension-popup-capture.md`; enforced by `popup_js_must_not_use_fetch_keepalive`).

### Data Model

`borg.yml` on the signing host. AMO signs one artifact per version (`borg/src/extension/sign.rs` reuse path, commit `87b5ac8`), so exactly one host signs; today that is lappy. Every host that installs that version gets the same baked list:

```yaml
extension:
  # Ordered. The popup tries each in turn; it moves on only when no HTTP
  # response came back (DNS, connect, timeout). Unset -> [http://localhost:8181].
  endpoints:
    - http://ripr.example.ts.net:8181
    - http://ripr.lan:8181
  # Per-attempt ceiling. Unset -> 3000.
  attempt-timeout-ms: 3000
  # origin-patterns: unchanged; endpoint hostnames are added on top of it.
```

`ExtensionConfig` (`borg/src/config.rs`):

```rust
pub struct ExtensionConfig {
    pub origin_patterns: Option<Vec<String>>,
    pub endpoints: Option<Vec<String>>,
    pub attempt_timeout_ms: Option<u64>,
}
```

- Validation at stage time, loud: each endpoint parses as a URL, scheme is `http`, host present, no path beyond `/`, no query string, no fragment, no `user:password@`. A trailing `/` is stripped. An `https` endpoint is an error: `host_permissions` and the CSP are `http://` only. (A query would make `endpoint + "/ingest"` post to the wrong path; credentials would ship to AMO.)
- An empty `endpoints: []` is an error, not a silent default.
- `attempt-timeout-ms: 0` is an error.
- 3000 ms default is a chosen UX ceiling, not a derived number. Inputs: the dead-peer hang measured 60s+; the ~17ms `/ingest` reply is historical (`fa79724`), not re-measured. `/ingest` does a synchronous intake write before it replies (`borg/src/routes.rs` `ingest`), and the receipts connection sets `busy_timeout` 5000 (`borg/src/receipts.rs`), so a contended intake can exceed 3s. That tail is the double-submit window in Security.

Generated `daemon.js` (values emitted with `serde_json`, never string-formatted):

```js
// Generated by `sb borg extension stage` from borg.yml extension.*. Do not edit.
const BORG_DAEMON = Object.freeze({
  endpoints: ["http://ripr.example.ts.net:8181", "http://ripr.lan:8181"],
  attemptTimeoutMs: 3000,
});
```

`chrome.storage.local` after this change: `authToken` only. `endpoint` is removed.

Config-hash sidecar, `web-ext-artifacts/<version>.config-hash`:

- Content: hex SHA-256 of the RENDERED config-derived files: `manifest.json` with its `version` field blanked, then `daemon.js`. Hashing the output, not the raw fields, covers every config input `stage()` reads, now and later: `extension.origin-patterns`, `extension.endpoints`, `extension.attempt-timeout-ms`, and `server.host` (merged into the patterns by `origin_patterns()` when `origin-patterns` is unset).
- Written by `sign::run` BEFORE the AMO upload. A sidecar with no `.xpi` is never read, so a failed upload leaves nothing harmful; the next sign overwrites it.
- On the reuse path (`locate_versioned_xpi` finds an `.xpi` for this version), `sign::run` re-renders and recomputes. Match -> reuse as today. Mismatch or missing sidecar -> error: `extension v<X> was signed with different extension config; bump the version to re-sign`.
- When a sidecar is missing:
  - an `.xpi` for the current version signed by a binary older than this change (only if the new binary runs `extension install` before `bump`; the Rollout Plan order avoids it)
  - an `.xpi` copied from another host without its sidecar (the `sign.rs` comment allows copying)
  - Refusing is correct in both: AMO already holds that version, so a bump is the only re-sign.
- A copied `.xpi` with its sidecar is checked against the RECEIVING host's `borg.yml`. It is reused only if that host renders the same files.
- Signing rule: first publisher wins. Whichever host signs a version first owns it on AMO; any other host gets AMO's `Version already exists` conflict. Today that host is lappy, because it is the only host where `--if-installed` proceeds. `sign::run`'s existing `web-ext sign failed with exit N` error gains the hint: `if AMO reported "already exists", another host published v<X> first; bump to re-sign`.
- Exit code: a sidecar mismatch makes `sb borg extension install` exit 3. The `otto deploy` extension step (`.otto.yml`, the `if sb borg extension install --no-policy --if-installed` block) treats exit 3 as fatal and fails the deploy; every other extension failure stays non-fatal as today. The daemons are already restarted by then, so failing only flags the stale extension.

### API Design

`fallback.js` (classic script, one global function, no DOM, no `chrome.*`):

```js
// Returns { ok: true, endpoint, response } on the first HTTP response,
// or { ok: false, attempts: [{ endpoint, error }] } when none answered.
async function postWithFallback({ endpoints, timeoutMs, path, headers, body, fetchImpl = fetch, onAttempt = () => {} })
```

Rules:

- Attempts run in list order, one at a time.
- Each attempt is `fetchImpl(endpoint + path, { method: "POST", headers, body, signal: AbortSignal.timeout(timeoutMs) })`.
- A rejected fetch (`TypeError` network error, `TimeoutError`/`AbortError`) records `{ endpoint, error: err.name + ": " + err.message }` and moves on.
- ANY response ends the walk, including 401, 4xx, 5xx. The request reached the daemon; a second route would only repeat the same answer or double-submit.
- No `keepalive`.

`popup.js` `capture()`:

- Before each attempt the status reads `Trying <hostname>...` (an `onAttempt(endpoint)` callback on `postWithFallback`).
- `ok && response.ok` -> status `Queued via <hostname>`, close after 400ms (as today).
- `ok && !response.ok` -> `fail()` with `Daemon error: HTTP <status> (<hostname>)`.
- `!ok` -> one `fail()` notification: `No response from <host1>: <error>; <host2>: <error>`. After a timeout, delivery is unknown (the daemon may have taken it), so the wording never says "unreachable".

`options.html` / `options.js`:

- The endpoint text input becomes a read-only list rendered from `BORG_DAEMON.endpoints`, with a line naming `extension.endpoints` in `~/.config/sb/borg.yml` as the place to change it.
- Save writes `authToken` and calls `chrome.storage.local.remove("endpoint")`.
- The `chrome.permissions.contains` check is dropped: the manifest now covers every endpoint by construction.
- `popup.js` never reads `endpoint`. A stale value (whatever lappy holds today) is inert until the next options save deletes it.

### Implementation Plan

#### Phase 0: Prove Firefox on lappy reaches the MagicDNS name
**Model:** none (operator spike, zero code)
- On lappy, in the daily Firefox profile, open `http://ripr.example.ts.net:8181/health` in a tab.
- Why: lappy's profile has DoH rollout prefs, and public DNS says `NXDOMAIN` for the name. Firefox must fall back to the system resolver (MagicDNS) or exclude the tailnet suffix from DoH. curl on lappy does not exercise Firefox's resolver.
- If it fails: add `example.ts.net` to `network.trr.excluded-domains` in `about:config`, retry, and record that step in the Rollout Plan before Phase 1 starts.
- **Success criteria:**
  - the tab renders the `/health` JSON (not a DNS error page, not an `https://` redirect)
  - Observed on main: not runnable from ripr; needs lappy's Firefox. curl on lappy returned `200`.

#### Phase 1: Config and manifest
**Model:** sonnet
- Add `endpoints` and `attempt_timeout_ms` to `ExtensionConfig` (`borg/src/config.rs`) with the doc comment updated.
- Add a validated accessor (`borg/src/extension/daemon.rs`): `resolved_endpoints(config) -> Result<Vec<Url>>` and `attempt_timeout_ms(config) -> Result<u64>`, applying the defaults and the validation rules above.
- `origin_patterns` (`borg/src/extension/manifest.rs`) appends each endpoint hostname not already covered (exact or wildcard, via `covered_by_wildcard`), after the explicit-or-default list is chosen.
- Tests in `borg/src/extension/manifest/tests.rs` and `borg/src/extension/daemon/tests.rs`:
  - endpoints only -> `http://ripr.example.ts.net/*` in `host_permissions` and `http://ripr.example.ts.net:*` in the CSP, defaults still present
  - explicit `origin-patterns` + endpoints -> endpoint hosts present
  - `ripr.lan` endpoint adds no pattern (covered by `*.lan`)
  - `https://`, empty list, unparseable URL, path, timeout 0 -> each an error
  - unset -> `[http://localhost:8181]`, 3000
- **Success criteria:**
  - `cargo test --workspace --lib extension::` passes, including the cases above (amended after the implementation audit: the `-p borg` form does not build because the vault `search` feature is only unified across the workspace; it fails at v0.22.0 too)
  - existing `default_host_permissions_match_default_origins` test passes unchanged (no endpoints set -> no new patterns)

#### Phase 2: Stage writes daemon.js
**Model:** sonnet
- `stage()` (`borg/src/extension.rs`) writes `daemon.js` from the Phase 1 accessor; a validation error fails the stage.
- Add `fallback.js` (empty stub until Phase 4) to `static_files`.
- `popup.html` and `options.html` load `daemon.js` and `fallback.js` before their own script.
- Extend `borg/tests/stage_produces_valid_extension_dir.rs`: `daemon.js` exists; its endpoints match config order; every endpoint hostname is covered by a `host_permissions` entry; the HTML files reference `daemon.js` before `popup.js`/`options.js`.
- **Success criteria:**
  - `sb borg extension stage --to <dir>` with the Data Model config writes `<dir>/daemon.js` listing `ripr.example.ts.net` before `ripr.lan`
    - Observed on main: stage writes no `daemon.js` (files: `icons ingest-schema.json manifest.json options.css options.html options.js popup.css popup.html popup.js`); `host_permissions` = `["http://localhost/*","http://*.lan/*","http://*.local/*"]`

#### Phase 3: Config-hash sidecar on sign
**Model:** sonnet
- `extension::daemon::baked_config_hash(config)`: render `manifest.json` (version blanked) and `daemon.js` through the same functions `stage()` uses, hash both.
- `sign::run` (`borg/src/extension/sign.rs`): write `<version>.config-hash` before the AMO upload; on the reuse path, compare and return a typed `ConfigMismatch` error on mismatch or a missing sidecar. Add the "already exists" hint to the `web-ext sign failed` error.
- `sb borg extension install` maps `ConfigMismatch` to exit 3.
- `.otto.yml` deploy: the extension step fails the deploy on exit 3, stays non-fatal otherwise.
- Unit tests: same config -> same hash; a change to endpoints, timeout, origin patterns, or `server.host` alone -> different hash; a version-only change -> same hash; reuse with a mismatched or missing sidecar -> `ConfigMismatch` naming `bump` (reuse path exercised against a temp artifacts dir holding a dummy versioned `.xpi`, no AMO call).
- **Success criteria:**
  - a reuse of an existing `.xpi` with a changed `extension.endpoints` returns an error containing `bump the version to re-sign`
    - Observed on main: the reuse path (`locate_versioned_xpi` in `borg/src/extension/sign.rs`) returns `Ok` and logs `reusing existing signed .xpi`, with no config check
  - break-test: making the reuse path skip the hash compare fails the mismatch test

#### Phase 4: Fallback walk and its tests
**Model:** opus
- Implement `postWithFallback` in `borg/clients/extension/fallback.js` per API Design.
- Rewire `popup.js` `capture()` to it; delete `getConfig()`'s endpoint default; read `authToken` only.
- First JS test harness in the repo: `borg/tests/js/fallback.test.mjs`, run with `node --test borg/tests/js/`. It loads `fallback.js` into a `node:vm` context (with node's `AbortSignal` passed in) and passes a mock `fetchImpl`. No package.json, no dependencies.
  - first endpoint rejects with `TypeError` -> second called, result names the second
  - first endpoint times out (mock rejects with the signal's reason when `signal` aborts, and never resolves otherwise; 50ms timeout) -> second called
  - first returns 500 -> second NOT called, result carries the 500
  - first returns 401 -> second NOT called
  - all reject -> `ok: false`, `attempts` lists both in order
- New otto task `extension-test` (`node --test borg/tests/js/`) added to `ci`'s `before`. `.github/workflows/ci.yml` runs in `container: debian:trixie`, which has no node: add `nodejs` to its `apt-get install` line and a step running `node --test borg/tests/js/`.
- `popup_js_must_not_use_fetch_keepalive` extended to scan `fallback.js`.
- **Success criteria:**
  - `otto extension-test` passes all five cases. Observed on main: `unknown task 'extension-test'`; `borg/tests/js` does not exist.
  - break-test: replacing the catch body with `throw err` (no fallback) makes the first two cases and the all-reject case fail

#### Phase 5: Options page
**Model:** sonnet
- `options.html`: replace the endpoint input with a read-only list; keep the token input.
- `options.js`: render `BORG_DAEMON.endpoints`; save writes `authToken` and removes `endpoint`; drop the permission check.
- Update `staged_options_js_builds_the_origin_from_hostname_not_host` (the check it pins is removed); replace it with a scan asserting no `"endpoint"` storage read remains.
- **Success criteria:**
  - `rg -n "[\"']endpoint[\"']" borg/clients/extension/*.js` returns exactly one line, the `chrome.storage.local.remove("endpoint")` call. Observed after Phase 4 (`4b18f56`): 2 lines, `options.js:2` (`getElementById("endpoint")`) and `options.js:6` (`storage.local.get(["endpoint", "authToken"])`).
  - Amended at build time: the original criterion, `rg -n '\bendpoint\b'` returning one line, contradicted this doc's API Design. `postWithFallback` returns `{ endpoint, response }` and `attempts: [{ endpoint, error }]` and calls `onAttempt(endpoint)`, so the bare word appears on 17 lines of `fallback.js` and `popup.js` by spec. The quoted-key form matches only the storage key and element id this phase removes.

#### Phase 6: Docs
**Model:** sonnet
- `borg/clients/AGENTS.md`: endpoint list from config, fallback rules, `daemon.js` is generated, JS tests live in `borg/tests/js/`. Reword the anti-pattern "Don't hardcode the daemon endpoint in distributed builds" to "the endpoint list comes from `extension.endpoints` at stage time; never a literal in source".
- `config/templates/borg.yml.example`: commented `extension:` block from the Data Model.
- `borg/AGENTS.md`: mention `daemon.rs` and the generated `daemon.js` where the extension module is described.
- **Success criteria:**
  - `rg -n 'extension.endpoints' config/templates/borg.yml.example borg/clients/AGENTS.md borg/AGENTS.md` returns at least one line per file, and `rg -n 'Don.t hardcode the daemon endpoint' borg/clients/AGENTS.md` returns no lines. Observed on main: first command returns no lines; second returns one.

## Acceptance Criteria

- [ ] With `extension.endpoints` set as in the Data Model, `sb borg extension stage --to <dir>` writes `daemon.js` listing `http://ripr.example.ts.net:8181` then `http://ripr.lan:8181`, and `manifest.json` `host_permissions` contains `http://ripr.example.ts.net/*`.
  - Observed on main: stage rc=0, no `daemon.js`, `host_permissions` = `["http://localhost/*","http://*.lan/*","http://*.local/*"]`
- [ ] `otto extension-test` exits 0 with the five Phase 4 cases, and `otto ci` runs it.
  - Observed on main: `unknown task 'extension-test'`
- [ ] Live on lappy, Tailscale up: a capture shows `Queued via ripr.example.ts.net`, and `sb borg log --since 10m --method http` on ripr lists its trace. With `sudo tailscale down` on lappy, on the home LAN: a capture shows `Queued via ripr.lan` and lands the same way.
  - Observed on main: `sb borg log --since 7d --method http` works (latest row `ht-870a7e39`, `succeeded`); the popup shows plain `Queued` with no host
- [ ] Live on lappy, with a test build whose first endpoint is a dead tailnet IP (`http://100.64.0.99:8181`) and second is `http://ripr.lan:8181`: a capture on the home LAN lands via `ripr.lan` after ~`attempt-timeout-ms`, the popup stays open through the wait, and `sb borg log --since 10m --method http` on ripr lists exactly one new trace.
  - Observed on main: `curl` to `100.64.0.99:8181` hangs past 60s; the walk itself is not runnable until Phase 4 ships
- [ ] On lappy, after a release is signed, changing only `extension.attempt-timeout-ms` in `borg.yml` and running `sb borg extension install --no-policy --if-installed` exits 3 with `bump the version to re-sign`, and `otto deploy` exits non-zero.
  - Observed on main: the reuse path returns `Ok` (`locate_versioned_xpi` in `borg/src/extension/sign.rs`, log `reusing existing signed .xpi`), and the `.otto.yml` extension step prints `⚠️  Extension refresh failed (non-fatal ...)` on any failure

## Resolved Decisions

- 2026-11-14, Scott: Tailscale is the primary route, LAN HTTP the fallback.
- 2026-11-14, Scott (option A, after panel round 1 found AMO's one-signature-per-version rule): the endpoint list lives in the signing host's `borg.yml`; sign enforces it with a config-hash sidecar. Rejected: list committed in the public repo (tailnet name goes public, addresses become source); options-page list in storage (hand config on every install).
- Panel round 2 dispositions (sidecar only):
  - folded: hash the rendered `manifest.json` + `daemon.js` (covers `server.host`); sidecar named by version and written before upload; missing-sidecar cases restated correctly; mismatch exits 3 and fails `otto deploy`; first-publisher-wins wording plus the AMO hint; copied-`.xpi` rule; sidecar acceptance criterion; ripr rollout step corrected
  - no action: `locate_versioned_xpi` uses `name.contains(version)` (`0.22.0` matches `10.22.0`) and reads "found 2" as "none". Neither happens with three-part bumps from one line of versions; pre-existing, not changed by this doc.
- Panel round 1 dispositions:
  - folded: CI container needs `nodejs`; timeout mock must honor `signal`; break-test mutation; options and docs phase criteria made falsifiable; URL validation (query, fragment, userinfo); `serde_json` emission; "No response from" wording; 3000ms labeled a choice with the 5s `busy_timeout` tail disclosed; AC5 now exercises the timeout path; focus-loss risk restated with a `Trying <host>...` status
  - no action: 6s worst-case walk (bounded, disclosed in Performance); extra popup-integration tests (the five `postWithFallback` cases cover the fallback boundary; the popup glue is three branches); redirects (the daemon issues none)

## Alternatives Considered

### Telegram as the fallback
- **Description:** on HTTP failure, the extension sends the URL to the borg bot.
- **Pros:** works from anywhere; borg already ingests Telegram.
- **Cons:** the bot only sees messages from a user; the Bot API cannot send as Scott, and bots never receive other bots' messages. It also puts a bot token in the extension.
- **Why not chosen:** it cannot work.

### ntfy as the fallback
- **Description:** POST to `ntfy.sh/<topic>`; `borg/src/ntfy.rs` already parses `{url, note}`.
- **Pros:** works from anywhere with internet; listener code exists.
- **Cons:** topic name is the only auth; every captured URL transits a public relay; needs an ntfy block on ripr and an `ntfy.sh` origin.
- **Why not chosen:** the tailnet already reaches ripr from anywhere, privately.

### Tailscale only, no LAN fallback
- **Description:** one endpoint, the MagicDNS name.
- **Pros:** no fallback code.
- **Cons:** a capture fails whenever tailscaled is down on lappy, even at home.
- **Why not chosen:** Scott asked for the LAN fallback.

### Keep the options-page endpoint as an override
- **Description:** storage `endpoints` beats the baked-in list.
- **Pros:** change the list without re-signing; one artifact serves hosts with different lists.
- **Cons:** two sources for one fact; lappy's stale `endpoint` key would beat config; a fresh install is still hand-configured.
- **Why not chosen:** Scott chose the signing-host config (2026-11-14). Endpoints change rarely; a change is a bump plus `otto deploy`.

### Endpoint list committed in the repo
- **Description:** one list in source, identical for every host.
- **Pros:** no per-host config; no sign-host coupling.
- **Cons:** the repo is public, so the tailnet name goes public; addresses become source, not config.
- **Why not chosen:** Scott chose the signing-host config (2026-11-14).

### Deliver the list through Firefox policy (`3rdparty` managed storage)
- **Description:** `sb borg extension install` writes the list into `policies.json`; the extension reads `storage.managed`.
- **Pros:** no generated file.
- **Cons:** written only on a sudo install, not by `otto deploy`'s `--no-policy --if-installed`; Firefox support for managed storage in this install mode is unverified.
- **Why not chosen:** `stage()` is the proven seam.

## Technical Considerations

### Dependencies

- No new crates. borg already depends on `url` (workspace dep, `borg/Cargo.toml`).
- Node for `otto extension-test`: local v26.10.0; GitHub CI's `debian:trixie` container gets `nodejs` from apt (Phase 4).

### Performance

- Happy path unchanged: one POST.
- Worst case before a notification: `len(endpoints) x attempt-timeout-ms`, 6s with the Data Model config. The popup stays open meanwhile, as it does today on a slow request.

### Security

- `daemon.js` goes to AMO with the signed xpi. It holds hostnames that `host_permissions` already ships today. The auth token is never baked in; it stays in `storage.local`.
- No new reachability: the daemon already listens on `0.0.0.0:8181`, reachable on the LAN and the tailnet.
- Double submit: a timeout can fire after the request was sent but before the response returned. The second route then submits again. The daemon writes a second receipt; the pipeline's `InflightGuard` turns it into `Duplicate` while the first still runs, or a reingest-and-replace if it already finished (no duplicate note, one extra LLM run). The window opens only when intake itself is slow: the receipts `busy_timeout` is 5000ms, above the 3000ms ceiling.

### Testing Strategy

- Rust unit tests for config validation and pattern merging (Phase 1).
- Rust integration test for the staged directory (Phase 2).
- Rust unit tests for the sign sidecar, reuse path only (Phase 3).
- Node tests for the fallback walk with a mock fetch (Phase 4), including a break-test.
- Live checks on lappy (Acceptance Criteria 4 and 5), with receipts confirmed on ripr.

### Rollout Plan

1. On lappy, before the release lands: add the `extension:` block from the Data Model to `~/.config/sb/borg.yml`.
2. Merge; `bump`.
3. On lappy: `otto deploy`. Its last step signs the new version with lappy's config, writes the sidecar and swaps the `.xpi`; Firefox picks it up.
4. On ripr: `otto deploy`. The extension step skips: `--if-installed` reads `/opt/firefox/distribution/policies.json`, which does not exist on ripr (a stale entry in `/etc/firefox/policies/policies.json` points at an old 0.15.14 `.xpi` and is not read by this check). ripr's `web-ext-artifacts/` is empty today.
5. Open the options page once and save, to delete the stale `endpoint` key (cosmetic: the popup no longer reads it).
6. Run Acceptance Criteria 4 and 5.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Firefox DoH resolves the MagicDNS name via Cloudflare (NXDOMAIN) and never asks MagicDNS | Med | High | Phase 0 proves it before code; `network.trr.excluded-domains` is the named fix |
| Timeout fires after the daemon received the POST, causing a double submit | Low | Low | `InflightGuard` dedupe; reingest replaces in place; tail is a contended intake under the 5s `busy_timeout` |
| A configured endpoint is missing from `host_permissions` | Low | High | Endpoint hosts are merged into the patterns by construction; Phase 2 test asserts coverage |
| Config changed without a bump; the old `.xpi` is reused with stale endpoints | Med | High | Config-hash sidecar over the rendered files; mismatch exits 3 and fails `otto deploy` |
| `sign` dies after the AMO upload, before the `.xpi` lands | Low | Low | Sidecar is written before upload; the version is published, so recovery is a bump, which the error names |
| A bad `borg.yml` entry ships a broken extension | Low | Med | Stage-time validation fails loudly; nothing is signed |
| Popup loses focus mid-walk (Scott clicks away during a 3s timeout) and the capture dies silently | Med | Med | New exposure: the no-keepalive argument (`popup.js` header) relied on a ~17ms reply. The popup status reads `Trying <host>...` during each attempt so a wait is visible; Acceptance Criterion 5 checks the popup survives the timeout path |
| Node missing on a CI runner | Low | Low | Phase 4 installs `nodejs` in the trixie container; otto task fails loudly if absent |

## Open Questions

(none)

## References

- `docs/design/2026-05-21-extension-lifecycle.md` (origin-patterns, Tailscale named as the use case)
- `docs/design/2026-05-22-extension-manifest-binary-versioned.md` (generated-at-stage constraint)
- `docs/design/2026-06-03-extension-popup-capture.md` (no background script, no keepalive)
- `docs/design/2026-06-07-extension-installer-post-snap-cleanup.md` (force-install does not seed storage)
- `borg/clients/AGENTS.md`
