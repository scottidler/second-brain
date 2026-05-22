# Firefox Extension: Signing & Install

How to ship a new version of the `obsidian-borg Capture` Firefox extension
(the toolbar button + Alt+Shift+B hotkey that POSTs the current tab URL to
the local borg daemon).

The extension source lives at `borg/clients/extension/`. Build / sign /
install / uninstall are owned by `sb borg extension <verb>`. There is no
shell-script entry point; both `bin/extension-sign` and the in-source
`sign.sh` were retired alongside the design at
`docs/design/2026-05-21-extension-lifecycle.md`.

## Prerequisites: AMO API credentials

Mozilla requires signed `.xpi`s for permanent install in stable Firefox.
The signing endpoint is the AMO (addons.mozilla.org) API and authenticates
with a per-user JWT issuer + secret pair.

1. Sign in to <https://addons.mozilla.org/en-US/developers/addon/api/key/>.
2. Generate (or copy) the API credentials. The page shows two values:
   - **JWT issuer** (looks like `user:NNNNNNNN:NNN`)
   - **JWT secret** (long hex string)
3. Export them with the `MOZILLA_` prefix the sign path reads:

   ```bash
   # ~/.zshenv or equivalent (or a secrets file you source)
   export MOZILLA_JWT_ISSUER="user:NNNNNNNN:NNN"
   export MOZILLA_JWT_SECRET="..."
   ```

The secret grants the ability to sign `.xpi`s under your AMO account
ONLY. It does not access browser data or installed extensions. If it leaks
(e.g. into a shell transcript), rotate it on the same AMO page - the old
key becomes invalid the moment a replacement is generated.

## Version: extension follows the workspace, automatically

The extension's `manifest.json` is **generated** from
`borg/src/extension/manifest.rs` with `version = env!("CARGO_PKG_VERSION")`.
Bumping the workspace via `bump` also bumps the extension. There is no
separate extension-version concept anymore. Drift between
`Cargo.toml`, `manifest.json`, and the signed `.xpi` is structurally
impossible: regenerate the manifest after `bump` (or rely on CI to catch
the omission via the `extension-validate` task in `otto ci`).

## First-machine setup (one time, requires sudo)

On a fresh machine that has never had the extension installed:

```bash
sudo sb borg extension install
# - or equivalently -
sudo sb bootstrap --extension
```

This:

1. Regenerates `manifest.json` + `ingest-schema.json`.
2. Calls `web-ext sign` against the AMO API (10-30s).
3. Symlinks `web-ext-artifacts/obsidian-borg-latest.xpi` at the freshly
   signed versioned `.xpi`.
4. Writes (or deep-merges into) the Firefox Enterprise Policy at the
   detected path:
   - Mozilla tarball (`/opt/firefox/`): `/opt/firefox/distribution/policies.json`
   - Ubuntu / Debian / Fedora apt-Firefox: `/etc/firefox/policies/policies.json`
   - Flatpak: `~/.var/app/org.mozilla.firefox/.mozilla/firefox/policies/policies.json`
   - Snap: explicitly rejected (snap confinement blocks `file://` install_url).
5. Firefox's file-watch on the `install_url` triggers an immediate install
   on next Firefox launch (no click-through prompt; `force_installed` mode).

Sudo is required ONLY for the policy file write on system paths
(`/etc/firefox/`, `/opt/firefox/`). The signing step runs as your user.

## Day-to-day: just `bump && otto deploy`

After the first-machine setup, the full development loop is:

```bash
bump            # bumps the workspace version (and therefore the extension)
otto deploy     # builds + installs sb + restarts daemons + refreshes the extension
```

The `otto deploy` task runs `sb borg extension install --no-policy
--if-installed` at the end of its sequence. This:

- Re-signs the extension (no sudo; the policy file already exists from
  the first-machine setup).
- Atomically swaps the `obsidian-borg-latest.xpi` symlink to the new
  versioned `.xpi`.
- Mozilla docs guarantee Firefox watches the file at `install_url` and
  auto-reinstalls when it changes. No Firefox restart needed.

The `--if-installed` flag makes the hook a no-op on daemon-only servers
(no Firefox, no sudo, no failure). The same `otto deploy` works
everywhere.

## Verifying what's installed

The cached `.xpi` lives in the active Firefox profile's `extensions/` dir.
For tarball Firefox:

```bash
unzip -p ~/.mozilla/firefox/*.default*/extensions/obsidian-borg@scottidler.xpi manifest.json | grep version
```

Or just open `about:addons` -> obsidian-borg Capture and read the version
line. That version is what's actually running, regardless of what the
on-disk `.xpi` symlink target says.

## Managed environments (Ansible / Puppet / Chef)

If `/etc/firefox/policies/policies.json` is owned by a config-management
agent, point our writer at a drop-in your management tool merges in:

```bash
sb borg extension install --policy-file /etc/firefox/policies.d/obsidian-borg.json
```

The deep-merge logic still runs against the override path; the management
agent's own policy entries are unaffected because we only own the
`policies.ExtensionSettings["obsidian-borg@scottidler"]` subtree of the
file we write.

## Uninstall

```bash
sb borg extension uninstall            # removes the policy entry
sb borg extension uninstall --purge    # also deletes web-ext-artifacts/
```

Firefox loads the new (or absent) policy on next launch; restart Firefox
to drop the running extension from the active profile.

## Drift detection

`otto ci` runs `sb borg extension validate` as a final gate. If
`manifest.json` or `ingest-schema.json` drifts from what
`borg/src/extension/manifest.rs` + `borg::types::IngestRequest` would
produce, the build fails with a short unified-diff snippet pointing at
the first divergence. Fix: `sb borg extension generate && git add
borg/clients/extension/manifest.json borg/clients/extension/ingest-schema.json`.

## Daemon-protocol evolution rule

`borg::types::IngestRequest` evolves additively-only. An `Option<>` field
is non-breaking; a required field is breaking. The integration test
`borg/tests/extension_body_matches_ingest_request.rs` deserializes the
canonical extension body (`{"url": "..."}`) into `IngestRequest` at CI
time; a required-field addition without an extension update fails CI
with an actionable message.

## Files involved

| File | Purpose |
|---|---|
| `borg/src/extension/manifest.rs` | Manifest generator (source of truth for `manifest.json`) |
| `borg/src/extension/schema.rs` | `IngestRequest` JSON Schema generator |
| `borg/src/extension/sign.rs` | `web-ext sign` wrapper |
| `borg/src/extension/install.rs` | Firefox detection + atomic symlink + policies.json deep-merge |
| `borg/clients/extension/manifest.json` | Generated; committed; CI-validated |
| `borg/clients/extension/ingest-schema.json` | Generated; committed; CI-validated |
| `borg/clients/extension/background.js` | Service worker - POSTs `{url}` to `/ingest` |
| `borg/clients/extension/options.js` | Save-time `chrome.permissions.contains` check against host_permissions |
| `borg/clients/extension/web-ext-artifacts/` | Signed `.xpi` output dir (gitignored) |
| `borg/tests/extension_body_matches_ingest_request.rs` | Build-time contract gate |
| `.otto.yml` -> `extension-validate` | CI drift gate |
| `.otto.yml` -> `deploy` -> last step | `--if-installed --no-policy` auto-refresh |
