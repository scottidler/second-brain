// The toolbar action opens this popup (manifest action.default_popup), and the
// keyboard shortcut opens it via the reserved `_execute_action` command. A
// popup is a fresh page on every invocation, so capture never depends on a
// long-lived background context being alive - that context could die mid-session
// and brick the silent onClicked path (see
// docs/design/2026-06-03-extension-popup-capture.md).
//
// The fetch does NOT set `keepalive: true` (that broke capture on snap Firefox).
// It DOES await a few chrome.storage round-trips before the POST: empirically,
// firing the fetch immediately on popup load (without those awaits) causes the
// request to die before it leaves the browser. The diag() breadcrumbs below
// provide both that settle time and a profile-readable trace of the capture path.

async function diag(step, extra) {
  try {
    const cur = (await chrome.storage.local.get("diag")).diag || [];
    cur.push(Object.assign({ t: new Date().toISOString(), step }, extra || {}));
    await chrome.storage.local.set({ diag: cur.slice(-50) });
  } catch (_) {
    // storage unavailable - nothing else we can do from here
  }
}

async function getEndpoint() {
  const data = await chrome.storage.local.get("endpoint");
  return data.endpoint || "http://localhost:8181";
}

async function capture() {
  const status = document.getElementById("status");
  await diag("loaded");

  let tab;
  try {
    [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  } catch (err) {
    await diag("tab-query-error", { msg: String((err && err.message) || err) });
    status.textContent = "tab query error";
    return;
  }
  await diag("tab-queried", {
    hasUrl: !!(tab && tab.url),
    url: tab && tab.url ? tab.url.slice(0, 100) : null,
  });
  if (!tab || !tab.url) {
    status.textContent = "No active tab URL";
    return;
  }

  const endpoint = await getEndpoint();
  await diag("fetching", { endpoint });
  try {
    const res = await fetch(`${endpoint}/ingest`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: tab.url }),
    });
    await diag("fetch-returned", { httpStatus: res.status, ok: res.ok });
    if (!res.ok) {
      status.textContent = `Daemon error: HTTP ${res.status}`;
      status.style.color = "#C62828";
      return;
    }
    const body = await res.json();
    await diag("queued", { trace: body && body.trace_id });
    status.textContent = `✓ Queued (${(body && body.trace_id) || "ok"})`;
    status.style.color = "#2E7D32";
    setTimeout(() => window.close(), 2000);
  } catch (err) {
    await diag("fetch-error", { msg: String((err && err.message) || err) });
    status.textContent = `Error: ${(err && err.message) || err}`;
    status.style.color = "#C62828";
  }
}

document.addEventListener("DOMContentLoaded", capture);
