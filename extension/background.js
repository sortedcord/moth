// Full page loads of watch URLs are redirected to handoff.html by the declarativeNetRequest rules
// (rules.json) before YouTube loads anything. This worker covers the rest: YouTube's in-page
// navigation (clicking a video on youtube.com changes the URL without a page load), and what the
// toolbar popup (popup.html) asks for: turning the whole thing on and off, and sharing the
// browser's YouTube session with moth as one of its accounts.

const HOST = "dev.moth.open";
const WATCH_HOSTS = new Set(["www.youtube.com", "youtube.com", "m.youtube.com"]);

/** Tabs where the user chose to watch in the browser; left alone until they load another page. */
const bypassedTabs = new Set();

function isWatchUrl(url) {
  try {
    const parsed = new URL(url);
    return WATCH_HOSTS.has(parsed.hostname) && parsed.pathname === "/watch" && parsed.searchParams.has("v");
  } catch {
    return false;
  }
}

async function isEnabled() {
  const { enabled = true } = await chrome.storage.local.get("enabled");
  return enabled;
}

async function applyEnabled(enabled) {
  await chrome.storage.local.set({ enabled });
  await chrome.declarativeNetRequest.updateEnabledRulesets(
    enabled ? { enableRulesetIds: ["watch"] } : { disableRulesetIds: ["watch"] },
  );
  await chrome.action.setBadgeText({ text: enabled ? "" : "off" });
  await chrome.action.setBadgeBackgroundColor({ color: "#6b7490" });
  await chrome.action.setTitle({
    title: enabled ? "moth: YouTube videos open in moth" : "moth: YouTube videos open in the browser",
  });
}

/** The browser's youtube.com session as a Cookie header (one value per name, `.youtube.com` first). */
async function youtubeSession() {
  const cookies = await chrome.cookies.getAll({ domain: "youtube.com" });
  const byName = new Map();
  for (const cookie of cookies) {
    if (!byName.has(cookie.name) || cookie.domain === ".youtube.com") byName.set(cookie.name, cookie);
  }
  if (!byName.has("SAPISID") && !byName.has("__Secure-3PAPISID")) {
    throw new Error("This browser isn't signed in to YouTube.");
  }
  return [...byName.values()].map((cookie) => `${cookie.name}=${cookie.value}`).join("; ");
}

async function shareAccount(role) {
  const reply = await chrome.runtime.sendNativeMessage(HOST, { account: role, cookies: await youtubeSession() });
  if (!reply?.ok) throw new Error(reply?.error ?? "moth didn't answer");
}

chrome.runtime.onMessage.addListener((message, _sender, respond) => {
  const handle = async () => {
    switch (message?.type) {
      case "getEnabled":
        return { enabled: await isEnabled() };
      case "setEnabled":
        await applyEnabled(Boolean(message.enabled));
        return { ok: true };
      case "shareAccount":
        await shareAccount(message.role);
        return { ok: true };
      default:
        return { ok: false, error: "unknown request" };
    }
  };
  handle().then(respond, (err) => respond({ ok: false, error: String(err.message ?? err) }));
  return true; // respond asynchronously
});

chrome.runtime.onInstalled.addListener(async () => applyEnabled(await isEnabled()));
chrome.runtime.onStartup.addListener(async () => applyEnabled(await isEnabled()));

chrome.webNavigation.onCommitted.addListener((details) => {
  if (details.frameId !== 0) return;
  if (details.url.includes("moth_bypass=1")) {
    bypassedTabs.add(details.tabId);
  } else {
    bypassedTabs.delete(details.tabId);
  }
});

chrome.tabs.onRemoved.addListener((tabId) => bypassedTabs.delete(tabId));

// Chromium refuses the rules.json redirect when the navigation was started by a page with an
// opaque origin (e.g. a script in an about:blank popup). Navigations the extension starts itself
// aren't subject to that check, so redo the redirect from here.
chrome.webNavigation.onErrorOccurred.addListener(async (details) => {
  if (details.frameId !== 0 || !details.error.includes("ERR_BLOCKED_BY_CLIENT")) return;
  const handoff = chrome.runtime.getURL("handoff.html");
  let watchUrl = null;
  if (details.url.startsWith(`${handoff}#`)) watchUrl = details.url.slice(handoff.length + 1);
  else if (isWatchUrl(details.url) && (await isEnabled())) watchUrl = details.url;
  if (!watchUrl) return;
  // Navigating right away loses the race with Chromium showing its error page, which cancels it.
  await new Promise((resolve) => setTimeout(resolve, 200));
  // `?blocked`: the error page is now in the tab's history too, so the handoff page skips it.
  await chrome.tabs.update(details.tabId, { url: `${handoff}?blocked#${watchUrl}` });
});

chrome.webNavigation.onHistoryStateUpdated.addListener(
  async (details) => {
    if (details.frameId !== 0 || bypassedTabs.has(details.tabId)) return;
    if (!isWatchUrl(details.url) || !(await isEnabled())) return;
    try {
      const reply = await chrome.runtime.sendNativeMessage(HOST, { url: details.url });
      if (!reply?.ok) throw new Error(reply?.error ?? "moth didn't answer");
      // Undo the in-page navigation so the browser doesn't play the video too.
      await chrome.tabs.goBack(details.tabId);
    } catch (err) {
      // Leave the video playing in the browser rather than losing it.
      console.warn("moth: couldn't hand off", details.url, err);
    }
  },
  { url: [{ hostSuffix: "youtube.com", pathEquals: "/watch" }] },
);
