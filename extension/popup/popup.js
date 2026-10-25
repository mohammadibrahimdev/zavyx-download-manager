// Popup logic: render media discovered on this tab + recent downloads, and
// send URLs to the host.

const versionEl = document.getElementById("version");
const statusEl = document.getElementById("host-status");
const manualUrlEl = document.getElementById("manual-url");
const sendDirectBtn = document.getElementById("send-direct");
const sendVideoBtn = document.getElementById("send-video");
const sendHlsBtn = document.getElementById("send-hls");
const discoveredCard = document.getElementById("discovered-card");
const mediaListEl = document.getElementById("media-list");
const mediaCountEl = document.getElementById("media-count");
const downloadListEl = document.getElementById("download-list");
const downloadsEmptyEl = document.getElementById("downloads-empty");
const openOptionsEl = document.getElementById("open-options");
const openHostAppEl = document.getElementById("open-host-app");

versionEl.textContent = "v" + (chrome.runtime.getManifest().version || "");

// --- Check host availability ---
async function checkHost() {
  try {
    const resp = await chrome.runtime.sendMessage({ type: "ping_host" });
    if (resp?.ok) {
      setStatus("ok", "host connected");
    } else {
      setStatus("error", "host offline");
    }
  } catch {
    setStatus("error", "host offline");
  }
}
function setStatus(state, text) {
  statusEl.className = "status " + state;
  statusEl.querySelector(".text").textContent = text;
}

// --- Render discovered media on this tab ---
async function renderMedia() {
  const resp = await chrome.runtime.sendMessage({ type: "popup_get_tab_media" });
  const items = resp?.items || [];
  if (items.length === 0) {
    discoveredCard.hidden = true;
    return;
  }
  discoveredCard.hidden = false;
  mediaCountEl.textContent = items.length;
  mediaListEl.innerHTML = "";
  items.forEach((item) => {
    const li = document.createElement("li");
    const kind = document.createElement("span");
    kind.className = "kind";
    kind.textContent = item.kind || "?";
    const url = document.createElement("span");
    url.className = "url";
    url.textContent = item.url;
    url.title = item.url;
    const btn = document.createElement("button");
    btn.textContent = "Send";
    btn.onclick = async () => {
      const msgType = item.kind === "hls" || item.kind === "dash"
        ? "add_hls"
        : "add_video";
      btn.disabled = true;
      const r = await chrome.runtime.sendMessage({
        type: "popup_send_to_host",
        payload: { type: msgType, url: item.url, page_url: item.pageUrl },
      });
      btn.disabled = false;
      if (!r?.ok) {
        btn.textContent = "✗";
        setTimeout(() => (btn.textContent = "Send"), 1200);
      } else {
        btn.textContent = "✓";
        setTimeout(() => (btn.textContent = "Send"), 1200);
        renderDownloads();
      }
    };
    li.appendChild(kind);
    li.appendChild(url);
    li.appendChild(btn);
    mediaListEl.appendChild(li);
  });
}

// --- Render recent downloads from the host ---
async function renderDownloads() {
  let resp;
  try {
    resp = await chrome.runtime.sendMessage({
      type: "popup_send_to_host",
      payload: { type: "list_downloads" },
    });
  } catch {
    downloadsEmptyEl.hidden = false;
    downloadsEmptyEl.textContent = "Host offline.";
    downloadListEl.innerHTML = "";
    return;
  }
  if (!resp?.ok) {
    downloadsEmptyEl.hidden = false;
    downloadsEmptyEl.textContent = "Host offline.";
    downloadListEl.innerHTML = "";
    return;
  }
  const items = resp.response?.items || [];
  if (items.length === 0) {
    downloadsEmptyEl.hidden = false;
    downloadListEl.innerHTML = "";
    return;
  }
  downloadsEmptyEl.hidden = true;
  downloadListEl.innerHTML = "";
  // Show most recent first.
  items.sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));
  for (const item of items.slice(0, 10)) {
    const li = document.createElement("li");
    const filename = document.createElement("span");
    filename.className = "filename";
    filename.textContent = item.filename || item.url.split("/").pop() || "?";
    filename.title = item.url;
    const state = document.createElement("span");
    state.className = "state " + (item.state || "");
    state.textContent = item.state || "?";
    const progress = document.createElement("span");
    progress.className = "progress";
    if (item.total) {
      const pct = ((item.written / item.total) * 100).toFixed(1) + "%";
      progress.textContent = pct;
    } else {
      progress.textContent = humanBytes(item.written || 0);
    }
    li.appendChild(filename);
    li.appendChild(state);
    li.appendChild(progress);
    downloadListEl.appendChild(li);
  }
}

function humanBytes(n) {
  if (n < 1024) return n + " B";
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + " KB";
  if (n < 1024 * 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MB";
  return (n / (1024 * 1024 * 1024)).toFixed(2) + " GB";
}

// --- Wire up the buttons ---
sendDirectBtn.onclick = async () => {
  const url = manualUrlEl.value.trim();
  if (!url) return;
  sendDirectBtn.disabled = true;
  const r = await chrome.runtime.sendMessage({
    type: "popup_send_to_host",
    payload: { type: "add_download", url },
  });
  sendDirectBtn.disabled = false;
  if (r?.ok) {
    manualUrlEl.value = "";
    renderDownloads();
  } else {
    alert("Failed: " + (r?.error || "unknown"));
  }
};
sendVideoBtn.onclick = async () => {
  const url = manualUrlEl.value.trim();
  if (!url) return;
  const r = await chrome.runtime.sendMessage({
    type: "popup_send_to_host",
    payload: { type: "add_video", url, page_url: undefined },
  });
  if (r?.ok) {
    manualUrlEl.value = "";
    renderDownloads();
  } else {
    alert("Failed: " + (r?.error || "unknown"));
  }
};
sendHlsBtn.onclick = async () => {
  const url = manualUrlEl.value.trim();
  if (!url) return;
  const r = await chrome.runtime.sendMessage({
    type: "popup_send_to_host",
    payload: { type: "add_hls", url, page_url: undefined },
  });
  if (r?.ok) {
    manualUrlEl.value = "";
    renderDownloads();
  } else {
    alert("Failed: " + (r?.error || "unknown"));
  }
};
openOptionsEl.onclick = (e) => {
  e.preventDefault();
  chrome.runtime.openOptionsPage();
};
openHostAppEl.onclick = async (e) => {
  e.preventDefault();
  // The host launches the app if needed and brings its window to the front.
  await chrome.runtime.sendMessage({
    type: "popup_send_to_host",
    payload: { type: "show_window" },
  });
};

manualUrlEl.addEventListener("keydown", (e) => {
  if (e.key === "Enter") sendDirectBtn.click();
});

checkHost();
renderMedia();
renderDownloads();
// Refresh periodically while popup is open.
setInterval(() => {
  renderMedia();
  renderDownloads();
}, 2000);
