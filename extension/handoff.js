// Landing page for watch URLs redirected by rules.json; the original URL is in the fragment.
const HOST = "dev.moth.open";
const url = location.hash.slice(1);

const title = document.getElementById("title");
const detail = document.getElementById("detail");
const browserButton = document.getElementById("browser");

function watchInBrowser() {
  const separator = url.includes("?") ? "&" : "?";
  location.replace(`${url}${separator}moth_bypass=1`);
}

async function leave() {
  // Back to where the user came from, or close the tab if it was opened just for this video.
  // After a blocked redirect (see background.js) Chromium's error page sits in between.
  const steps = location.search === "?blocked" ? 2 : 1;
  if (history.length > steps) {
    history.go(-steps);
    return;
  }
  const tab = await chrome.tabs.getCurrent();
  if (tab?.id !== undefined) await chrome.tabs.remove(tab.id);
}

async function handOff() {
  detail.textContent = url;
  try {
    const reply = await chrome.runtime.sendNativeMessage(HOST, { url });
    if (!reply?.ok) throw new Error(reply?.error ?? "moth didn't answer");
    title.textContent = "Opened in moth";
    await leave();
  } catch (err) {
    title.textContent = "Couldn't open this video in moth";
    detail.textContent = String(err.message ?? err);
    detail.classList.add("error");
    browserButton.hidden = false;
  }
}

browserButton.addEventListener("click", watchInBrowser);
handOff();
