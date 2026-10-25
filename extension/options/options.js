// Options page logic. Persists to chrome.storage.local and sends config to host.

const HOST_DEFAULT = "io.github.mohammadibrahimdev.zavyx_dm";

const el = (id) => document.getElementById(id);
const hostNameEl = el("host-name");
const hostStatusEl = el("host-status");
const downloadDirEl = el("download-dir");
const maxConcurrentEl = el("max-concurrent");
const maxSegmentsEl = el("max-segments");
const overlayEnabledEl = el("overlay-enabled");
const overlayYouTubeEl = el("overlay-youtube");
const ctxLinkEl = el("ctx-link");
const ctxPageEl = el("ctx-page");
const ctxAllEl = el("ctx-all");
const saveBtn = el("save-config");
const reloadBtn = el("reload-config");

async function loadFromStorage() {
  const data = await chrome.storage.local.get([
    "hostName", "overlayEnabled", "overlayYouTube",
    "ctxLink", "ctxPage", "ctxAll",
  ]);
  hostNameEl.value = data.hostName || HOST_DEFAULT;
  overlayEnabledEl.checked = data.overlayEnabled ?? true;
  overlayYouTubeEl.checked = data.overlayYouTube ?? true;
  ctxLinkEl.checked = data.ctxLink ?? true;
  ctxPageEl.checked = data.ctxPage ?? true;
  ctxAllEl.checked = data.ctxAll ?? true;
}

async function loadFromHost() {
  try {
    const resp = await chrome.runtime.sendMessage({
      type: "popup_send_to_host",
      payload: { type: "get_config" },
    });
    if (resp?.ok && resp.response?.config) {
      const cfg = resp.response.config;
      downloadDirEl.value = cfg.download_dir || "";
      maxConcurrentEl.value = cfg.max_concurrent_downloads || 4;
      maxSegmentsEl.value = cfg.max_segments || 8;
      hostStatusEl.textContent = "connected";
      hostStatusEl.className = "ok";
    } else {
      hostStatusEl.textContent = "host offline — settings will sync when host runs";
      hostStatusEl.className = "error";
    }
  } catch {
    hostStatusEl.textContent = "host offline — settings will sync when host runs";
    hostStatusEl.className = "error";
  }
}

async function saveAll() {
  await chrome.storage.local.set({
    hostName: hostNameEl.value.trim() || HOST_DEFAULT,
    overlayEnabled: overlayEnabledEl.checked,
    overlayYouTube: overlayYouTubeEl.checked,
    ctxLink: ctxLinkEl.checked,
    ctxPage: ctxPageEl.checked,
    ctxAll: ctxAllEl.checked,
  });
  // Push config to host.
  const resp = await chrome.runtime.sendMessage({
    type: "popup_send_to_host",
    payload: {
      type: "set_config",
      download_dir: downloadDirEl.value.trim() || null,
      max_concurrent_downloads: parseInt(maxConcurrentEl.value) || null,
      max_segments: parseInt(maxSegmentsEl.value) || null,
    },
  });
  if (resp?.ok) {
    saveBtn.textContent = "Saved ✓";
    setTimeout(() => (saveBtn.textContent = "Save"), 1500);
  } else {
    alert("Saved locally but host rejected: " + (resp?.error || "unknown"));
  }
}

saveBtn.onclick = saveAll;
reloadBtn.onclick = loadFromHost;

loadFromStorage();
loadFromHost();
