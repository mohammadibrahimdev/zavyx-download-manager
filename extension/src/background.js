// Background service worker (Manifest V3).
//
// Responsibilities:
//   1. Media sniffer — watch webRequest events for video/audio stream URLs
//      (m3u8, mp4, webm, mpd, etc.) and notify the host / popup.
//   2. Context menu — "Download with Zavyx DM" on links, videos, audio, pages.
//   3. Native Messaging bridge — exchange JSON messages with the Zavyx DM host app.
//   4. Per-tab media cache — accumulate URLs discovered in each tab so the
//      popup can list them.

// --- Native Messaging port to the host app ---
const HOST_NAME = "io.github.mohammadibrahimdev.zavyx_dm";
let port = null;
let portConnecting = false;
let pendingMessages = []; // Messages to flush once port opens.
let messageHandlers = new Map(); // msgId -> resolve/reject callbacks.

function connectNative() {
  if (portConnecting || port) return;
  portConnecting = true;
  try {
    port = chrome.runtime.connectNative(HOST_NAME);
    port.onMessage.addListener(onNativeMessage);
    port.onDisconnect.addListener(() => {
      const reason = chrome.runtime.lastError?.message || "Zavyx DM is not installed or could not start";
      console.warn("[Zavyx DM] Native port disconnected:", reason);
      port = null;
      portConnecting = false;
      failPending(reason);
      // Reconnect after a short delay so the link comes back once Zavyx DM is installed/fixed.
      setTimeout(connectNative, 5000);
    });
    // Flush pending messages.
    for (const msg of pendingMessages) {
      port.postMessage(msg);
    }
    pendingMessages = [];
  } catch (e) {
    console.error("[Zavyx DM] Failed to connect to native host:", e);
    port = null;
    portConnecting = false;
    failPending(String(e?.message || e));
  }
}

// Reject every request still waiting for the host, so callers don't hang.
function failPending(reason) {
  pendingMessages = [];
  for (const { reject } of messageHandlers.values()) {
    reject(new Error(reason));
  }
  messageHandlers.clear();
}

function sendToHost(message) {
  // Promise-based: returns the next matching response (with the same `seq`).
  return new Promise((resolve, reject) => {
    const seq = Math.random().toString(36).slice(2);
    const wrapped = { seq, ...message };
    // Register before sending: a failed connect rejects pending handlers immediately.
    messageHandlers.set(seq, { resolve, reject });
    if (!port) {
      pendingMessages.push(wrapped);
      connectNative();
    } else {
      try {
        port.postMessage(wrapped);
      } catch (e) {
        pendingMessages.push(wrapped);
        connectNative();
      }
    }
    // Wait up to 30s for a response with this seq.
    setTimeout(() => {
      if (messageHandlers.has(seq)) {
        messageHandlers.delete(seq);
        reject(new Error("Timeout waiting for host response"));
      }
    }, 30000);
  });
}

function onNativeMessage(msg) {
  if (!msg) return;
  // If the host echoes `seq`, route to the matching handler.
  if (msg.seq && messageHandlers.has(msg.seq)) {
    const { resolve, reject } = messageHandlers.get(msg.seq);
    messageHandlers.delete(msg.seq);
    if (msg.type === "error") {
      reject(new Error(msg.message || "host error"));
    } else {
      resolve(msg);
    }
    return;
  }
  // Otherwise broadcast to listeners (popup etc.).
  chrome.runtime.sendMessage({ type: "host_event", payload: msg }).catch(() => {
    // popup not open — fine
  });
}

// --- Per-tab discovered media cache ---
const tabMedia = new Map(); // tabId -> [{ url, kind, mime, pageUrl, ts }]

// Requests that look like media but aren't useful to download: site UI sounds,
// and the small chunks streaming players fetch (the whole video is reachable via
// the page URL or the playlist instead).
function isNoise(url) {
  return (
    /youtube\.com\/s\//i.test(url) ||
    /googlevideo\.com\/videoplayback/i.test(url) ||
    /\.(ts|m4s|aac)(\?|#|$)/i.test(url) ||
    /[?&](range|bytestart|sq)=/i.test(url)
  );
}

function recordMedia(tabId, item) {
  if (tabId == null || tabId < 0 || isNoise(item.url)) return;
  if (!tabMedia.has(tabId)) tabMedia.set(tabId, []);
  const list = tabMedia.get(tabId);
  // Dedupe by URL.
  if (list.some(i => i.url === item.url)) return;
  list.push(item);
  // Cap to 50 per tab to avoid runaway memory.
  if (list.length > 50) list.shift();
  // Notify popup if open.
  chrome.runtime.sendMessage({
    type: "media_discovered",
    tabId,
    item,
  }).catch(() => {});
}

function getTabMedia(tabId) {
  return tabMedia.get(tabId) || [];
}

// --- Media sniffer via webRequest ---
const MEDIA_EXT_RE = /\.(mp4|webm|mkv|avi|m4v|mov|m3u8|mpd|ts|m4a|mp3|wav|flac|flv)(\?|#|$)/i;
const MEDIA_MIME_RE = /^(video|audio|application\/vnd\.apple\.mpegurl|application\/dash\+xml|x-mpegurl)/i;

chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (details.tabId < 0) return; // service worker / prefetch
    const url = details.url;
    let kind = null;
    if (/\.(m3u8)(\?|#|$)/i.test(url)) kind = "hls";
    else if (/\.(mpd)(\?|#|$)/i.test(url)) kind = "dash";
    else if (MEDIA_EXT_RE.test(url)) kind = "video";
    else return; // not media

    recordMedia(details.tabId, {
      url,
      kind,
      pageUrl: undefined, // we'll fill in below
      ts: Date.now(),
    });
    // Also fetch the page URL for context.
    chrome.tabs.get(details.tabId).then((tab) => {
      const list = tabMedia.get(details.tabId);
      if (list) {
        const last = list[list.length - 1];
        if (last && last.url === url) last.pageUrl = tab.url;
      }
    }).catch(() => {});
  },
  { urls: ["<all_urls>"] }
);

chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const mime = (details.responseHeaders || []).find(
      h => h.name.toLowerCase() === "content-type"
    )?.value;
    if (!mime) return;
    if (MEDIA_MIME_RE.test(mime)) {
      recordMedia(details.tabId, {
        url: details.url,
        kind: mime.includes("mpegurl") ? "hls" :
              mime.includes("dash+xml") ? "dash" : "media",
        mime,
        ts: Date.now(),
      });
    }
  },
  { urls: ["<all_urls>"] },
  ["responseHeaders"]
);

// Clean up per-tab cache when a tab closes.
chrome.tabs.onRemoved.addListener((tabId) => {
  tabMedia.delete(tabId);
});

// --- Context menu ---
chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({
    id: "zavyx-dm-download-link",
    title: "Download with Zavyx DM",
    contexts: ["link", "image", "video", "audio"],
  });
  chrome.contextMenus.create({
    id: "zavyx-dm-download-page",
    title: "Download page with Zavyx DM",
    contexts: ["page"],
  });
  chrome.contextMenus.create({
    id: "zavyx-dm-download-selection",
    title: "Download selected URLs with Zavyx DM",
    contexts: ["selection"],
  });
  chrome.contextMenus.create({
    id: "zavyx-dm-download-all-videos",
    title: "Download all media on this page",
    contexts: ["page", "video"],
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  let url = null;
  let kind = null;
  switch (info.menuItemId) {
    case "zavyx-dm-download-link":
      url = info.linkUrl || info.srcUrl || info.pageUrl;
      kind = "direct";
      break;
    case "zavyx-dm-download-page":
      url = info.pageUrl;
      kind = "page";
      break;
    case "zavyx-dm-download-selection":
      // Selection may contain multiple URLs.
      if (info.selectionText) {
        const urls = extractUrls(info.selectionText);
        for (const u of urls) {
          await sendToHost({ type: "add_download", url: u });
        }
        return;
      }
      break;
    case "zavyx-dm-download-all-videos":
      const items = getTabMedia(tab.id);
      for (const item of items) {
        await sendToHost({
          type: item.kind === "hls" ? "add_hls" :
                item.kind === "dash" ? "add_hls" :
                "add_video",
          url: item.url,
          page_url: item.pageUrl || tab.url,
        });
      }
      return;
  }
  if (!url) return;
  // Decide routing: known video site -> add_video, .m3u8 -> add_hls, else direct.
  if (isKnownVideoSite(url)) {
    await sendToHost({ type: "add_video", url, page_url: tab?.url });
  } else if (/\.m3u8(\?|#|$)/i.test(url) || /\.mpd(\?|#|$)/i.test(url)) {
    await sendToHost({ type: "add_hls", url, page_url: tab?.url });
  } else {
    await sendToHost({ type: "add_download", url, referer: tab?.url });
  }
});

// --- Routing: content script messages ---
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || !msg.type) return;
  switch (msg.type) {
    case "popup_get_tab_media":
      chrome.tabs.query({ active: true, currentWindow: true }).then((tabs) => {
        const tabId = tabs[0]?.id;
        sendResponse({ items: tabId ? getTabMedia(tabId) : [] });
      });
      return true; // async
    case "popup_send_to_host":
      sendToHost(msg.payload).then(
        (resp) => sendResponse({ ok: true, response: resp }),
        (err) => sendResponse({ ok: false, error: err.message })
      );
      return true; // async
    case "content_video_click":
      // User clicked "Download this video" overlay on a <video>.
      sendToHost({
        type: "add_video",
        url: msg.url,
        page_url: sender.tab?.url,
        kind: msg.kind,
        format: msg.format,
      }).then(
        (resp) => sendResponse({ ok: true, response: resp }),
        (err) => sendResponse({ ok: false, error: err.message })
      );
      return true;
    case "content_sniffed_video":
      // Content script found a <video src=...> on the page.
      recordMedia(sender.tab?.id, {
        url: msg.url,
        kind: "video",
        pageUrl: sender.tab?.url,
        ts: Date.now(),
      });
      return;
    case "ping_host":
      sendToHost({ type: "ping" }).then(
        (resp) => sendResponse({ ok: true, response: resp }),
        // A one-shot call reports the browser's exact reason (host not found,
        // forbidden, exited...), which the long-lived port often doesn't.
        (err) =>
          chrome.runtime.sendNativeMessage(HOST_NAME, { type: "ping" }, (resp) => {
            const reason = chrome.runtime.lastError?.message;
            if (resp && !reason) sendResponse({ ok: true, response: resp });
            else sendResponse({ ok: false, error: reason || err.message });
          })
      );
      return true;
  }
});

// --- Helpers ---
function isKnownVideoSite(url) {
  try {
    const u = new URL(url);
    const h = u.hostname.toLowerCase();
    return [
      "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com",
      "twitch.tv", "tiktok.com", "instagram.com", "twitter.com",
      "x.com", "facebook.com", "v.qq.com", "bilibili.com",
      "pinterest.com", "reddit.com", "soundcloud.com",
    ].some(s => h.includes(s));
  } catch {
    return false;
  }
}

function extractUrls(text) {
  const out = [];
  const re = /https?:\/\/[^\s<>"]+?[^\s<>".]/gi;
  let m;
  while ((m = re.exec(text))) out.push(m[0]);
  return out;
}

// Initial connect attempt (will retry on failure).
connectNative();
