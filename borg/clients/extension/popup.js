// The toolbar action opens this popup (manifest action.default_popup), and the
// keyboard shortcut opens it via the reserved `_execute_action` command. A popup
// is a fresh page on every invocation, so capture never depends on a long-lived
// background context staying alive (see
// docs/design/2026-06-03-extension-popup-capture.md).
//
// Correctness requirements:
//   1. NO `keepalive: true` on the fetch. On snap Firefox 150.x a keepalive POST
//      to http://localhost never reaches the daemon (zero receipts, zero daemon
//      log) - the toolbar click silently produces no ingestion. Found empirically
//      in 1c3deb0, wrongly "restored per spec" in 4556577, re-confirmed broken
//      2026-06-06. The daemon is fire-and-forget (returns "Queued" in ~17ms, see
//      fa79724), so the POST completes before the popup can lose focus; the
//      focus-loss-abort case keepalive guarded against does not occur in practice.
//      DO NOT re-add keepalive - the design doc was wrong on this and is corrected.
//   2. Check `res.ok` - fetch does not reject on 4xx/5xx.
//   3. No scheme filter - guard only on `!tab.url`, forward any scheme
//      (including file://) to the daemon.

async function getConfig() {
  const data = await chrome.storage.local.get(["endpoint", "authToken"]);
  return {
    endpoint: data.endpoint || "http://localhost:8181",
    authToken: data.authToken || "",
  };
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
  if (!tab || !tab.url) {                  // guard only on missing URL; do NOT filter by scheme
    status.textContent = "No active tab URL";
    return;
  }
  const { endpoint, authToken } = await getConfig();
  const headers = { "Content-Type": "application/json" };
  if (authToken) {                         // optional Bearer; daemon requires it only when configured
    headers["Authorization"] = `Bearer ${authToken}`;
  }
  try {
    // IngestRequest gained an optional `note` capture-annotation field
    // (Phase 8, docs/design/2026-07-05-distillation-knowledge-extraction.md).
    // The toolbar popup captures a bare tab URL with no annotation surface, so
    // `note` is omitted here; it deserializes to None server-side. If a future
    // popup adds a note textarea, send it as `note` in this body.
    const res = await fetch(`${endpoint}/ingest`, {
      method: "POST",
      headers,
      body: JSON.stringify({ url: tab.url }),
    });                                    // NO keepalive - see requirement 1 above
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
