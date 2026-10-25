// Content script: shows a "Download" button over every <video> on the page
// (with a quality menu), and reports plain <video src> URLs to the background
// worker so the popup can list them.
//
//   - Hovering a video fades the button in at its top-right corner.
//   - "Download" sends the video to Zavyx in the best quality; the ▾ arrow
//     opens a menu with 1080p / 720p / 480p / audio only.
//   - blob:/MediaSource players (YouTube etc.) hide the real stream, so the page
//     URL is sent instead and yt-dlp resolves it.

(function () {
  "use strict";

  if (window.__zavyx_injected) return;
  window.__zavyx_injected = true;

  const QUALITIES = [
    ["Best quality", ""],
    ["1080p", "bv*[height<=1080]+ba/b[height<=1080]"],
    ["720p", "bv*[height<=720]+ba/b[height<=720]"],
    ["480p", "bv*[height<=480]+ba/b[height<=480]"],
    ["Audio only", "bestaudio/best"],
  ];


  /** @type {Map<HTMLVideoElement, HTMLDivElement>} */
  const overlays = new Map();
  let hideTimer = null;

  function videoSource(video) {
    let url = video.currentSrc || video.src || video.querySelector("source")?.src;
    if (!url || !/^https?:/i.test(url)) return { url: location.href, kind: "page" };
    return { url, kind: undefined };
  }

  function send(video, overlay, format) {
    const { url, kind } = videoSource(video);
    const label = overlay.querySelector(".zavyx-label");
    overlay.classList.remove("zavyx-menu-open");
    label.textContent = "Sending…";
    chrome.runtime
      .sendMessage({ type: "content_video_click", url, kind, format: format || undefined })
      .then((r) => {
        overlay.classList.add(r?.ok ? "zavyx-ok" : "zavyx-fail");
        label.textContent = r?.ok ? "Added to Zavyx" : "Zavyx not connected";
      })
      .catch(() => {
        overlay.classList.add("zavyx-fail");
        label.textContent = "Zavyx not connected";
      })
      .finally(() => {
        setTimeout(() => {
          overlay.classList.remove("zavyx-ok", "zavyx-fail");
          label.textContent = "Download";
        }, 2200);
      });
  }

  function ensureOverlay(video) {
    if (overlays.has(video)) return overlays.get(video);
    const overlay = document.createElement("div");
    overlay.className = "zavyx-video-overlay";
    overlay.innerHTML = `
      <div class="zavyx-btn-group">
        <button type="button" class="zavyx-main" title="Download with Zavyx">
          <span class="zavyx-z">Z</span><span class="zavyx-label">Download</span>
        </button>
        <button type="button" class="zavyx-more" title="Choose quality" aria-haspopup="true">▾</button>
      </div>
      <div class="zavyx-menu" role="menu">
        ${QUALITIES.map(([name, fmt]) => `<button type="button" role="menuitem" data-format="${fmt}">${name}</button>`).join("")}
      </div>`;
    const stop = (e) => {
      e.preventDefault();
      e.stopPropagation();
    };
    overlay.addEventListener("mousedown", (e) => e.stopPropagation());
    overlay.querySelector(".zavyx-main").addEventListener("click", (e) => {
      stop(e);
      send(video, overlay, "");
    });
    overlay.querySelector(".zavyx-more").addEventListener("click", (e) => {
      stop(e);
      overlay.classList.toggle("zavyx-menu-open");
    });
    overlay.querySelectorAll(".zavyx-menu button").forEach((b) =>
      b.addEventListener("click", (e) => {
        stop(e);
        send(video, overlay, b.dataset.format);
      })
    );
    overlay.addEventListener("mouseenter", () => clearTimeout(hideTimer));
    overlay.addEventListener("mouseleave", () => scheduleHide(video));
    document.documentElement.appendChild(overlay);
    overlays.set(video, overlay);
    return overlay;
  }

  function positionOverlay(video, overlay) {
    const rect = video.getBoundingClientRect();
    if (rect.width < 160 || rect.height < 90) {
      overlay.classList.remove("zavyx-visible");
      return false;
    }
    const width = overlay.offsetWidth || 150;
    overlay.style.top = `${window.scrollY + rect.top + 12}px`;
    overlay.style.left = `${window.scrollX + rect.right - width - 12}px`;
    return true;
  }

  function showOverlay(video) {
    clearTimeout(hideTimer);
    const overlay = ensureOverlay(video);
    overlays.forEach((o, v) => v !== video && o.classList.remove("zavyx-visible", "zavyx-menu-open"));
    if (positionOverlay(video, overlay)) overlay.classList.add("zavyx-visible");
  }

  function scheduleHide(video) {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      const overlay = overlays.get(video);
      if (overlay && !overlay.matches(":hover")) overlay.classList.remove("zavyx-visible", "zavyx-menu-open");
    }, 1200);
  }

  // Report plain <video src> URLs so the popup can offer them.
  function scanVideos() {
    document.querySelectorAll("video").forEach((v) => {
      const { url, kind } = videoSource(v);
      if (kind !== "page") chrome.runtime.sendMessage({ type: "content_sniffed_video", url }).catch(() => {});
    });
  }

  // Many players put controls on top of the <video>, so track the pointer
  // position rather than relying on the video getting the hover itself.
  let lastMove = 0;
  document.addEventListener(
    "mousemove",
    (e) => {
      const now = Date.now();
      if (now - lastMove < 120) return;
      lastMove = now;
      for (const v of document.querySelectorAll("video")) {
        const r = v.getBoundingClientRect();
        if (e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom) {
          showOverlay(v);
          return;
        }
      }
      overlays.forEach((_, v) => scheduleHide(v));
    },
    { passive: true, capture: true }
  );

  const reposition = () =>
    overlays.forEach((overlay, video) => {
      if (overlay.classList.contains("zavyx-visible")) positionOverlay(video, overlay);
    });
  window.addEventListener("resize", reposition);
  let scrollRaf = null;
  window.addEventListener(
    "scroll",
    () => {
      if (scrollRaf) return;
      scrollRaf = requestAnimationFrame(() => {
        scrollRaf = null;
        reposition();
      });
    },
    { passive: true }
  );

  // Drop overlays whose video left the page (SPA navigation).
  setInterval(() => {
    overlays.forEach((overlay, video) => {
      if (!video.isConnected) {
        overlay.remove();
        overlays.delete(video);
      }
    });
  }, 3000);

  setTimeout(scanVideos, 800);
  let lastUrl = location.href;
  setInterval(() => {
    if (location.href !== lastUrl) {
      lastUrl = location.href;
      setTimeout(scanVideos, 1200);
    }
  }, 1500);
})();
