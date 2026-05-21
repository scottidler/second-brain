# Firefox Extension: Signing & Install

How to ship a new version of the `obsidian-borg Capture` Firefox extension
(the toolbar button that POSTs the current tab URL to the local borg daemon).

The extension source lives at `borg/clients/extension/`. The repo ships
build/sign/install automation via `bin/extension-sign`, surfaced as the
`otto extension` task.

## Prerequisites: AMO API credentials

Mozilla requires signed `.xpi`s for permanent install in stable Firefox. The
signing endpoint is the AMO (addons.mozilla.org) API and authenticates with a
per-user JWT issuer + secret pair.

1. Sign in to https://addons.mozilla.org/en-US/developers/addon/api/key/
2. Generate (or copy) your API credentials. The page shows two values:
   - **JWT issuer** (looks like `user:NNNNNNNN:NNN`)
   - **JWT secret** (long hex string)
3. Export them in your shell init with the `MOZILLA_` prefix the repo's
   `sign.sh` reads:

   ```bash
   # ~/.zshenv or equivalent (or a secrets file you source)
   export MOZILLA_JWT_ISSUER="user:NNNNNNNN:NNN"
   export MOZILLA_JWT_SECRET="..."
   ```

The secret grants the ability to sign `.xpi`s under your AMO account ONLY.
It does not access browser data or installed extensions. If it leaks (e.g.
into a shell transcript), rotate it on the same AMO page - the old key
becomes invalid the moment a replacement is generated.

## Bumping the extension version

The extension has its own version line in `borg/clients/extension/manifest.json`
that is DISTINCT from the workspace's `CARGO_PKG_VERSION` / git tag. You bump it
when you ship a meaningful change to the extension itself - new permissions,
behavioral changes to `background.js`, new options pages, etc. It does NOT
need to track every daemon release.

```json
{
  ...
  "version": "0.4.0"   // bump this when you change the extension
}
```

After bumping, sign and install. AMO refuses to re-sign an unchanged version;
each sign requires a fresh version number.

## Sign + install workflow

The fast path - sign and immediately install via `xdg-open`:

```bash
otto extension --install
# - or equivalently -
bin/extension-sign --install
```

This:

1. Validates `MOZILLA_JWT_ISSUER` + `MOZILLA_JWT_SECRET` are set.
2. Runs `web-ext sign` against the AMO API. Validation takes 10-30s.
3. Downloads the signed `.xpi` to
   `borg/clients/extension/web-ext-artifacts/c8b5da7dc30043ceb5b1-<version>.xpi`.
4. `xdg-open`s the `.xpi`. Firefox registers as the handler for
   `application/x-xpinstall` and pops the install-confirmation dialog.
5. Click **Continue Installation** in Firefox. Done.

Sign without auto-install (useful for CI or building a release artifact you'll
distribute):

```bash
otto extension
# Prints the path to the new .xpi.
```

## Verifying what's installed

The cached `.xpi` lives in the active Firefox profile's `extensions/` dir.
On Linux with snap Firefox:

```bash
unzip -p ~/snap/firefox/common/.mozilla/firefox/*.default*/extensions/obsidian-borg@scottidler.xpi manifest.json | grep version
```

Or check `about:addons` -> obsidian-borg Capture -> version line at the top.

The version printed there is what's actually running, regardless of what
`borg/clients/extension/manifest.json` says in the repo.

## When extension and daemon drift

The extension talks to the borg daemon over HTTP at `/ingest`. The contract
is small (URL POST in, queued response back), but it has historically drifted:

- Before fa79724 (2026-05-12), `/ingest` blocked until the pipeline finished
  and returned `{title, status, ...}`. The extension rendered toasts from the
  response.
- After fa79724, `/ingest` returns `{status: "Queued", ...}` within ms; the
  daemon's `notify::Desktop` sink delivers terminal toasts. The extension's
  `if (result.title)` and `else if (result.status.Failed)` branches went dead
  silently - extension 0.3.4 had them, extension 0.4.0 removes them.

How to tell you've drifted:

- Daemon log shows ingest succeeded but no terminal toast appeared:
  upgrade the extension (the dead-branch fall-through swallowed the response).
- Browser shows a "Captured: <stale-title>" toast: the daemon got upgraded
  past fa79724 but the extension is still pre-0.4.0.

If in doubt, run `otto extension --install` to ship the latest source as a
fresh signed `.xpi`.

## Files involved

| File | Purpose |
|---|---|
| `borg/clients/extension/manifest.json` | extension metadata; bump `version` here before signing |
| `borg/clients/extension/background.js` | service worker - handles toolbar click, POSTs to `/ingest` |
| `borg/clients/extension/sign.sh` | thin wrapper around `web-ext sign` |
| `bin/extension-sign` | repo-level entry point (called by `otto extension`); adds the `--install` shortcut |
| `borg/clients/extension/web-ext-artifacts/` | signed `.xpi` output dir; signed builds are committed for historical reference |
| `.otto.yml` -> `extension` task | `otto extension [--install]` |
