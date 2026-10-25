// Content script: injects a "Download this video" button overlay on every
// <video> element on the page, and reports discovered <video> src URLs to the
// background service worker.
//
// Behavior (mimics IDM's overlay):
//   - On mouseover a <video>, fade in a small floating button at top-right
//     of the video.
//   - Button has Zavyx branding (a download arrow).
//   - Click sends the video URL to the host via the background worker.
//   - For blob: URLs (common on YouTube), we resolve the actual media URL
//     via the background's media sniffer cache.

(function () {
  "use strict";

  // Avoid running in subframes repeatedly.
  if (window !== window.top && !window.__zavyx_injected) {
    // OK to run in subframes too — many video sites embed players in iframes.
  }
  if (window.__zavyx_injected) return;
  window.__zavyx_injected = true;

  /** @type {Map<HTMLVideoElement, HTMLDivElement>} */
  const overlays = new Map();

  function ensureOverlay(video) {
    if (overlays.has(video)) return overlays.get(video);
    const overlay = document.createElement("div");
    overlay.className = "zavyx-video-overlay";
    overlay.innerHTML = `
      <div class="zavyx-vbtn" role="button" tabindex="0" title="Download with Zavyx">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
          <path d="M5 20h14v-2H5v2zm7-18l-5 5h3v6h4v-6h3l-5-5z"/>
        </svg>
        <span>Download</span>
      </div>`;
    overlay.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      let url = video.currentSrc || video.src;
      if (!url) {
        const source = video.querySelector("source");
        url = source?.src;
      }
      // blob:/MediaSource players (YouTube etc.) hide the real stream, so hand
      // the page URL to yt-dlp instead.
      let kind;
      if (!url || !/^https?:/i.test(url)) {
        url = location.href;
        kind = "page";
      }
      chrome.runtime.sendMessage({
        type: "content_video_click",
        url,
        kind,
      });
      // Brief confirmation flash.
      overlay.classList.add("zavyx-clicked");
      setTimeout(() => overlay.classList.remove("zavyx-clicked"), 800);
    });
    // Attach to the document body so it floats above everything.
    document.body.appendChild(overlay);
    overlays.set(video, overlay);
    positionOverlay(video, overlay);
    // Hide by default.
    overlay.classList.remove("zavyx-visible");
    return overlay;
  }

  function positionOverlay(video, overlay) {
    const rect = video.getBoundingClientRect();
    overlay.style.top = `${window.scrollY + rect.top + 8}px`;
    overlay.style.left = `${window.scrollX + rect.right - overlay.offsetWidth - 8}px`;
  }

  function showOverlay(video) {
    const overlay = ensureOverlay(video);
    positionOverlay(video, overlay);
    overlay.classList.add("zavyx-visible");
  }

  function hideOverlay(video) {
    const overlay = overlays.get(video);
    if (overlay) overlay.classList.remove("zavyx-visible");
  }

  // Initial scan — find existing videos and report them.
  function scanVideos() {
    const videos = document.querySelectorAll("video");
    videos.forEach((v) => {
      let url = v.currentSrc || v.src;
      if (!url) {
        const source = v.querySelector("source");
        url = source?.src;
      }
      if (url && url.startsWith("http")) {
        chrome.runtime.sendMessage({ type: "content_sniffed_video", url });
      }
      ensureOverlay(v);
    });
  }

  // Mouse listeners on document (one handler per page, not per video).
  document.addEventListener("mouseover", (e) => {
    const v = e.target.closest("video");
    if (v) showOverlay(v);
  }, true);
  document.addEventListener("mouseout", (e) => {
    const v = e.target.closest("video");
    if (v && !v.matches("video:hover")) hideOverlay(v);
  }, true);

  // Watch for new <video> elements via MutationObserver.
  const observer = new MutationObserver((mutations) => {
    for (const mut of mutations) {
      mut.addedNodes.forEach((node) => {
        if (node.nodeType !== 1) return;
        if (node.tagName === "VIDEO") ensureOverlay(node);
        else node.querySelectorAll?.("video").forEach(ensureOverlay);
      });
    }
  });
  observer.observe(document.documentElement || document.body, {
    childList: true,
    subtree: true,
  });

  // Initial scan after a short delay (videos added after DOM ready).
  setTimeout(scanVideos, 800);
  // Re-scan on YouTube-style SPA navigation.
  let lastUrl = location.href;
  setInterval(() => {
    if (location.href !== lastUrl) {
      lastUrl = location.href;
      setTimeout(scanVideos, 1200);
    }
  }, 1500);

  // Reposition overlays on resize/scroll.
  window.addEventListener("resize", () => {
    overlays.forEach((overlay, video) => positionOverlay(video, overlay));
  });
  let scrollRaf = null;
  window.addEventListener("scroll", () => {
    if (scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = null;
      overlays.forEach((overlay, video) => positionOverlay(video, overlay));
    });
  }, { passive: true });
})();
