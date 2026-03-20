function renderResult(container, result) {
  container.textContent = "";
  if (result.title) {
    container.className = "status ok";
    const title = document.createElement("strong");
    title.textContent = result.title;
    container.appendChild(title);
    if (result.tags && result.tags.length) {
      const tags = document.createElement("div");
      tags.className = "tags";
      tags.textContent = result.tags.map(t => `#${t}`).join(", ");
      container.appendChild(tags);
    }
    if (result.folder) {
      const folder = document.createElement("div");
      folder.className = "tags";
      folder.textContent = `Folder: ${result.folder}`;
      container.appendChild(folder);
    }
  } else if (result.status && result.status.Failed) {
    container.className = "status err";
    container.textContent = `Failed: ${result.status.Failed.reason}`;
  } else {
    container.className = "status err";
    container.textContent = "Failed";
  }
}

document.addEventListener("DOMContentLoaded", async () => {
  const resultDiv = document.getElementById("result");
  const data = await chrome.storage.local.get("lastResult");

  if (data.lastResult) {
    renderResult(resultDiv, data.lastResult);
  }

  document.getElementById("capture").addEventListener("click", async () => {
    const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (!tab || !tab.url) return;

    const endpoint = (await chrome.storage.local.get("endpoint")).endpoint || "http://localhost:8181";
    resultDiv.className = "status none";
    resultDiv.textContent = "Sending...";

    try {
      const response = await fetch(`${endpoint}/ingest`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ url: tab.url }),
      });
      const result = await response.json();
      await chrome.storage.local.set({ lastResult: result });
      renderResult(resultDiv, result);
    } catch (err) {
      resultDiv.className = "status err";
      resultDiv.textContent = `Error: ${err.message}`;
    }
  });
});
