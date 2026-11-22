// The toolbar popup: the on/off switch for opening videos in moth, and sharing this browser's
// YouTube session with moth as one of its accounts.

const enabled = document.getElementById("enabled");
const status = document.getElementById("status");

function show(text, kind = "") {
  status.textContent = text;
  status.className = kind;
}

chrome.runtime.sendMessage({ type: "getEnabled" }, (reply) => {
  enabled.checked = reply?.enabled ?? true;
});
enabled.addEventListener("change", () => chrome.runtime.sendMessage({ type: "setEnabled", enabled: enabled.checked }));

for (const button of document.querySelectorAll("button[data-role]")) {
  button.addEventListener("click", async () => {
    const buttons = document.querySelectorAll("button[data-role]");
    buttons.forEach((b) => (b.disabled = true));
    show("Sharing…");
    try {
      const reply = await chrome.runtime.sendMessage({ type: "shareAccount", role: button.dataset.role });
      if (!reply?.ok) throw new Error(reply?.error ?? "moth didn't answer");
      show("Sent to moth; it shows the result in its settings.", "ok");
    } catch (err) {
      show(String(err.message ?? err), "error");
    } finally {
      buttons.forEach((b) => (b.disabled = false));
    }
  });
}
