const DEFAULT_ENDPOINT = "http://localhost:8181";

async function getEndpoint() {
  const data = await chrome.storage.local.get("endpoint");
  return data.endpoint || DEFAULT_ENDPOINT;
}

async function captureTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || !tab.url) {
    chrome.notifications.create({ type: "basic", iconUrl: "icons/icon-48.png", title: "obsidian-borg", message: "No active tab URL" });
    return;
  }

  const endpoint = await getEndpoint();
  try {
    // `/ingest` is fire-and-forget since fa79724 (2026-05-12): the response
    // returns `{status: "Queued", trace_id: ..., ...}` within milliseconds
    // and the daemon owns the lifecycle from there. Terminal notifications
    // (Saved/Duplicate/Failed) are delivered by the daemon's desktop sink,
    // not by the extension. The only failure the daemon by definition
    // cannot deliver is "borg unreachable" - that's the `catch` branch.
    const response = await fetch(`${endpoint}/ingest`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ url: tab.url }),
    });
    const result = await response.json();
    await chrome.storage.local.set({ lastResult: result });
  } catch (err) {
    chrome.action.setBadgeText({ text: "ERR", tabId: tab.id });
    chrome.action.setBadgeBackgroundColor({ color: "#F44336", tabId: tab.id });
    chrome.notifications.create({ type: "basic", iconUrl: "icons/icon-48.png", title: "obsidian-borg", message: `Error: ${err.message}` });
  }

  setTimeout(() => chrome.action.setBadgeText({ text: "", tabId: tab.id }), 3000);
}

// Toolbar click
chrome.action.onClicked.addListener(captureTab);

// Keyboard shortcut
chrome.commands.onCommand.addListener((command) => {
  if (command === "capture-url") captureTab();
});

// Auto-discover on install
chrome.runtime.onInstalled.addListener(async () => {
  try {
    const response = await fetch(`${DEFAULT_ENDPOINT}/health`);
    const data = await response.json();
    if (data.service === "obsidian-borg") {
      await chrome.storage.local.set({ endpoint: DEFAULT_ENDPOINT });
    }
  } catch {
    chrome.runtime.openOptionsPage();
  }
});
