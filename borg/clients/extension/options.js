document.addEventListener("DOMContentLoaded", async () => {
  const input = document.getElementById("endpoint");
  const msg = document.getElementById("msg");

  const data = await chrome.storage.local.get("endpoint");
  input.value = data.endpoint || "http://localhost:8181";

  document.getElementById("save").addEventListener("click", async () => {
    const endpoint = input.value.replace(/\/+$/, "");
    await chrome.storage.local.set({ endpoint });
    msg.textContent = "Saved!";
    setTimeout(() => { msg.textContent = ""; }, 2000);
  });
});
