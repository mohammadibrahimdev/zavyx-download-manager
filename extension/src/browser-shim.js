// Cross-browser API shim. Load this BEFORE any other extension script.
//
// In Manifest V3 both Chromium and Firefox expose a Promise-based `chrome.*`
// namespace, so no wrapping is needed — and wrapping is harmful: modern Chrome
// and Edge also define `browser`, and adding callbacks to methods such as
// `runtime.connectNative` breaks their signatures. We only make sure `chrome`
// exists.
(function () {
  "use strict";
  if (typeof chrome === "undefined" && typeof browser !== "undefined") {
    globalThis.chrome = browser;
  }
})();
