// The toolbar action opens this popup (manifest action.default_popup), and the
// keyboard shortcut opens it via the reserved `_execute_action` command. A popup
// is a fresh page on every invocation, so capture never depends on a long-lived
// background context staying alive (see
// docs/design/2026-06-03-extension-popup-capture.md).
//
// Three correctness requirements from that design doc:
//   1. `keepalive: true` on the fetch - a popup is destroyed the instant it loses
//      focus, and `keepalive` is what lets the POST finish through that unload.
//      Awaiting the response only covers the programmed close, not focus-loss.
//   2. Check `res.ok` - fetch does not reject on 4xx/5xx.
//   3. No scheme filter - mirror background.js: guard only on `!tab.url`, forward
//      any scheme (including file://) to the daemon.

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
  if (!tab || !tab.url) {                  // mirror background.js guard; do NOT filter by scheme
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
