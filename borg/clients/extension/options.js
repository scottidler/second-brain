document.addEventListener("DOMContentLoaded", async () => {
  const list = document.getElementById("endpoints");
  const tokenInput = document.getElementById("auth-token");
  const msg = document.getElementById("msg");

  for (const address of BORG_DAEMON.endpoints) {
    const item = document.createElement("li");
    item.textContent = address;
    list.appendChild(item);
  }

  const data = await chrome.storage.local.get(["authToken"]);
  tokenInput.value = data.authToken || "";

  document.getElementById("save").addEventListener("click", async () => {
    await chrome.storage.local.set({ authToken: tokenInput.value.trim() });
    await chrome.storage.local.remove("endpoint");
    msg.textContent = "Saved.";
    setTimeout(() => { msg.textContent = ""; }, 2000);
  });
});
