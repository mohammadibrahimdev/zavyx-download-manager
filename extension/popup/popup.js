// Popup: connection status, "download this video" for video pages, a single
// link box that routes to the right downloader, media found on the page, and
// recent downloads from the Zavyx DM app.

const $ = (id) => document.getElementById(id);
const statusEl = $("host-status");

$("version").textContent = "v" + chrome.runtime.getManifest().version;

// Same list the background uses to decide what yt-dlp should handle.
const VIDEO_SITES = [
  "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "twitch.tv",
  "tiktok.com", "instagram.com", "twitter.com", "x.com", "facebook.com",
  "fb.watch", "bilibili.com", "pinterest.com", "reddit.com", "soundcloud.com",
];

function isVideoSite(url) {
  try {
    const h = new URL(url).hostname.toLowerCase();
    return VIDEO_SITES.some((s) => h === s || h.endsWith("." + s));
  } catch {
    return false;
  }
}

function isStream(url) {
  return /\.(m3u8|mpd)(\?|#|$)/i.test(url);
}

function toHost(payload) {
  return chrome.runtime.sendMessage({ type: "popup_send_to_host", payload });
}

/** Run `action` from a button, showing ✓ / ✗ feedback on it. */
async function withButton(btn, action) {
  const label = btn.innerHTML;
  btn.disabled = true;
  let r;
  try {
    r = await action();
  } catch (e) {
    r = { ok: false, error: e.message };
  }
  btn.disabled = false;
  btn.classList.add(r?.ok ? "done" : "failed");
  btn.textContent = r?.ok ? "✓ Sent to Zavyx DM" : "✗ Failed";
  if (!r?.ok) btn.title = r?.error || "";
  setTimeout(() => {
    btn.classList.remove("done", "failed");
    btn.innerHTML = label;
  }, 1600);
  if (r?.ok) renderDownloads();
  return r;
}

// --- Connection status ---
async function checkHost() {
  let resp;
  try {
    resp = await chrome.runtime.sendMessage({ type: "ping_host" });
  } catch {}
  const ok = !!resp?.ok;
  statusEl.className = "status " + (ok ? "ok" : "error");
  statusEl.querySelector(".text").textContent = ok ? "Connected" : "Not connected";
  statusEl.title = ok ? "" : resp?.error || "";
  $("offline-notice").hidden = ok;
  $("offline-reason").textContent = ok ? "" : "Reason: " + (resp?.error || "no answer from the extension");
}

// --- Current tab: offer "download this video" on video sites ---
let currentTab = null;
async function renderTab() {
  [currentTab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!currentTab?.url || !isVideoSite(currentTab.url)) return;
  $("video-card").hidden = false;
  $("video-title").textContent = currentTab.title || "This video";
  $("video-title").title = currentTab.title || "";
}

$("download-video").onclick = () =>
  withButton($("download-video"), () =>
    toHost({
      type: "add_video",
      url: currentTab.url,
      page_url: currentTab.url,
      kind: "page",
      format: $("video-quality").value || undefined,
    })
  );

// --- Link box ---
function routeFor(url) {
  if (isVideoSite(url)) return { type: "add_video", kind: "page", label: "Video page — saved with the best quality" };
  if (isStream(url)) return { type: "add_hls", label: "Stream — saved as .mp4" };
  return { type: "add_download", label: "File — multi-connection download" };
}

$("manual-url").addEventListener("input", () => {
  const url = $("manual-url").value.trim();
  const hint = $("url-hint");
  hint.classList.remove("error");
  hint.textContent = /^https?:\/\//i.test(url) ? routeFor(url).label : "";
});

$("send-url").onclick = async () => {
  const url = $("manual-url").value.trim();
  if (!/^https?:\/\//i.test(url)) {
    $("url-hint").textContent = "Enter a link that starts with http:// or https://";
    $("url-hint").classList.add("error");
    return;
  }
  const { type, kind } = routeFor(url);
  const r = await withButton($("send-url"), () => toHost({ type, url, kind, page_url: currentTab?.url }));
  if (r?.ok) {
    $("manual-url").value = "";
    $("url-hint").textContent = "";
  }
};

$("manual-url").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("send-url").click();
});

// --- Media found on this page ---
function nameFromUrl(url) {
  try {
    const u = new URL(url);
    const last = decodeURIComponent(u.pathname.split("/").filter(Boolean).pop() || "");
    return last || u.hostname;
  } catch {
    return url;
  }
}

async function renderMedia() {
  const resp = await chrome.runtime.sendMessage({ type: "popup_get_tab_media" });
  const items = resp?.items || [];
  $("media-card").hidden = items.length === 0;
  $("media-count").textContent = items.length;
  const list = $("media-list");
  list.replaceChildren();
  for (const item of items) {
    const li = document.createElement("li");
    const kind = document.createElement("span");
    kind.className = "kind";
    kind.textContent = item.kind === "hls" || item.kind === "dash" ? "stream" : item.kind || "media";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = nameFromUrl(item.url);
    name.title = item.url;
    const btn = document.createElement("button");
    btn.textContent = "Download";
    btn.onclick = () =>
      withButton(btn, () =>
        toHost(
          item.kind === "hls" || item.kind === "dash"
            ? { type: "add_hls", url: item.url, page_url: item.pageUrl }
            : { type: "add_download", url: item.url, referer: item.pageUrl }
        )
      );
    li.append(kind, name, btn);
    list.appendChild(li);
  }
}

// --- Recent downloads from the app ---
function humanBytes(n) {
  if (!n) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(n) / Math.log(1024)));
  return (n / 1024 ** i).toFixed(i ? 1 : 0) + " " + units[i];
}

async function renderDownloads() {
  let resp;
  try {
    resp = await toHost({ type: "list_downloads" });
  } catch {}
  const items = resp?.ok ? resp.response?.items || [] : [];
  const list = $("download-list");
  list.replaceChildren();
  $("downloads-empty").hidden = items.length > 0;
  $("downloads-empty").textContent = resp?.ok ? "No downloads yet." : "Connect Zavyx DM to see your downloads.";
  items.sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));
  for (const item of items.slice(0, 6)) {
    const li = document.createElement("li");
    const dl = document.createElement("div");
    dl.className = "dl";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = item.filename || nameFromUrl(item.url);
    name.title = item.url;
    dl.appendChild(name);
    const state = document.createElement("span");
    state.className = "state " + item.state;
    if (item.state === "downloading" && item.total) {
      const pct = Math.min(100, (item.written / item.total) * 100);
      const bar = document.createElement("div");
      bar.className = "bar";
      const fill = document.createElement("span");
      fill.style.width = pct.toFixed(1) + "%";
      bar.appendChild(fill);
      dl.appendChild(bar);
      state.textContent = pct.toFixed(0) + "%";
    } else if (item.state === "completed") {
      state.textContent = "✓ " + humanBytes(item.total || item.written);
    } else {
      state.textContent = item.state;
    }
    li.append(dl, state);
    list.appendChild(li);
  }
}

// --- Footer ---
$("open-options").onclick = (e) => {
  e.preventDefault();
  chrome.runtime.openOptionsPage();
};
$("open-app").onclick = async (e) => {
  e.preventDefault();
  await toHost({ type: "show_window" });
  window.close();
};

checkHost();
renderTab();
renderMedia();
renderDownloads();
setInterval(() => {
  renderMedia();
  renderDownloads();
}, 2000);
