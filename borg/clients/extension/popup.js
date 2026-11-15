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
//      The POST itself lives in fallback.js, which carries the same ban.
//   2. Check `response.ok` - fetch does not reject on 4xx/5xx.
//   3. No scheme filter - guard only on `!tab.url`, forward any scheme
//      (including file://) to the daemon.
//
// The daemon addresses and the per-attempt timeout come from BORG_DAEMON
// (daemon.js, generated at stage time from borg.yml extension.*); the walk over
// them is postWithFallback (fallback.js).

async function getAuthToken() {
  const data = await chrome.storage.local.get(["authToken"]);
  return data.authToken || "";
}

function hostOf(endpoint) {
  return new URL(endpoint).hostname;
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

// Pure body builder: `note` is left out when empty after trimming, so an empty
// textarea is a bare capture. Its test is the Rust contract
// borg/tests/extension_body_matches_ingest_request.rs plus the stage check.
function buildBody(url, rawNote) {
  const body = { url };
  const note = (rawNote || "").trim();
  if (note) {
    body.note = note;
  }
  return body;
}

async function capture() {
  const status = document.getElementById("status");
  const noteField = document.getElementById("note");
  const button = document.getElementById("capture");
  button.disabled = true;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.url) {                  // guard only on missing URL; do NOT filter by scheme
    status.textContent = "No active tab URL";
    button.disabled = false;
    return;
  }
  const authToken = await getAuthToken();
  const headers = { "Content-Type": "application/json" };
  if (authToken) {                         // optional Bearer; daemon requires it only when configured
    headers["Authorization"] = `Bearer ${authToken}`;
  }
  try {
    const result = await postWithFallback({
      endpoints: BORG_DAEMON.endpoints,
      timeoutMs: BORG_DAEMON.attemptTimeoutMs,
      path: "/ingest",
      headers,
      body: JSON.stringify(buildBody(tab.url, noteField.value)),
      onAttempt: (endpoint) => {
        status.textContent = `Trying ${hostOf(endpoint)}...`;
      },
    });
    if (!result.ok) {                      // no endpoint answered; after a timeout delivery is unknown
      const detail = result.attempts.map((a) => `${hostOf(a.endpoint)}: ${a.error}`).join("; ");
      fail(status, `No response from ${detail}`);
      button.disabled = false;
      return;
    }
    const host = hostOf(result.endpoint);
    if (!result.response.ok) {             // fetch does not reject on 4xx/5xx
      fail(status, `Daemon error: HTTP ${result.response.status} (${host})`);
      button.disabled = false;
      return;
    }
    await result.response.json();
    status.textContent = `Queued via ${host}`;
    setTimeout(() => window.close(), 400);
  } catch (err) {
    fail(status, `Error: ${err.message}`);  // e.g. an unparseable response body
    button.disabled = false;
  }
}

// The popup no longer posts on open: Enter in the textarea or the Capture
// button submits, and Shift+Enter inserts a newline.
document.addEventListener("DOMContentLoaded", () => {
  document.getElementById("capture").addEventListener("click", capture);
  document.getElementById("note").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      capture();
    }
  });
});
