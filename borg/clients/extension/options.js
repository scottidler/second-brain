document.addEventListener("DOMContentLoaded", async () => {
  const input = document.getElementById("endpoint");
  const tokenInput = document.getElementById("auth-token");
  const msg = document.getElementById("msg");

  const data = await chrome.storage.local.get(["endpoint", "authToken"]);
  input.value = data.endpoint || "http://localhost:8181";
  tokenInput.value = data.authToken || "";

  document.getElementById("save").addEventListener("click", async () => {
    const value = input.value.trim().replace(/\/+$/, "");
    const authToken = tokenInput.value.trim();
    let url;
    try {
      url = new URL(value);
    } catch {
      msg.textContent = `Not a valid URL: ${value}`;
      return;
    }
    // Firefox match patterns reject a port (bugs 1362809, 1468162): use hostname, not host.
    const origin = `${url.protocol}//${url.hostname}/*`;
    const allowed = await chrome.permissions.contains({ origins: [origin] });
    if (!allowed) {
      msg.textContent =
        `Origin ${url.origin} is not in the extension's host_permissions. ` +
        `Add to extension.origin-patterns in ~/.config/sb/borg.yml and re-run sb borg extension install.`;
      return;
    }
    await chrome.storage.local.set({ endpoint: value, authToken });
    msg.textContent = "Saved.";
    setTimeout(() => { msg.textContent = ""; }, 2000);
  });
});
