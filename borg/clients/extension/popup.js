// The toolbar action opens this popup (manifest action.default_popup), and the
// keyboard shortcut opens it via the reserved `_execute_action` command. A
// popup is a fresh page on every invocation, so capture never depends on a
// long-lived background context being alive - that context could die mid-session
// and brick the silent onClicked path (see
// docs/design/2026-06-03-extension-popup-capture.md).

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
  if (!tab || !tab.url) {
    // Mirror background.js's historical guard exactly; do NOT filter by scheme
    // (that would silently drop file:// ingestion the daemon currently accepts).
    status.textContent = "No active tab URL";
    return;
  }

  const endpoint = await getEndpoint();
  try {
    const response = await fetch(`${endpoint}/ingest`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: tab.url }),
      // Complete the POST even if the popup closes on focus loss; without this
      // a focus-loss close aborts the in-flight request at the TCP level.
      keepalive: true,
    });
    if (!response.ok) {
      // fetch does not reject on HTTP 4xx/5xx - a daemon-side error with a JSON
      // body would otherwise masquerade as success.
      fail(status, `Daemon error: HTTP ${response.status}`);
      return;
    }
    await response.json();
    status.textContent = "Queued";
    // Brief confirmation, then self-close. Terminal Saved/Duplicate/Failed
    // arrives via the daemon's desktop notification sink, unchanged.
    setTimeout(() => window.close(), 400);
  } catch (err) {
    fail(status, `Error: ${err.message}`);
  }
}

document.addEventListener("DOMContentLoaded", capture);
