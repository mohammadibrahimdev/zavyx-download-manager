// Cross-browser API shim. Firefox exposes `browser.*` with Promise-returning
// methods; Chromium exposes `chrome.*` with callback-style methods (most of which
// also support Promises in MV3). This file normalises everything to `chrome.*`
// with Promise return types.
//
// Usage: include this BEFORE any other extension script. Then all other scripts
// can use `chrome.*` uniformly and expect Promises.

(function () {
  "use strict";
  if (typeof browser !== "undefined") {
    // Firefox — alias browser -> chrome (browser already returns Promises).
    if (typeof chrome === "undefined") {
      // Should not happen in modern Firefox, but be safe.
      globalThis.chrome = browser;
    } else {
      // Merge: prefer chrome.* if defined, fall back to browser.*.
      for (const k of Object.keys(browser)) {
        if (!(k in chrome)) chrome[k] = browser[k];
      }
    }
    // In Firefox, chrome.* callbacks are NOT Promises — wrap them.
    const wrap = (obj, keys) => {
      for (const key of keys) {
        if (obj && obj[key] && typeof obj[key].addListener === "function") {
          // Event — leave as-is.
          continue;
        }
        const orig = obj && obj[key];
        if (typeof orig === "function") {
          obj[key] = function (...args) {
            // If last arg is a callback, call through directly.
            if (args.length && typeof args[args.length - 1] === "function") {
              return orig.apply(this, args);
            }
            // Otherwise return a Promise.
            return new Promise((resolve, reject) => {
              orig.call(this, ...args, (result) => {
                const err = chrome.runtime.lastError;
                if (err) reject(err);
                else resolve(result);
              });
            });
          };
        }
      }
    };
    // We only shim the most-used namespaces.
    if (chrome.runtime) wrap(chrome.runtime, ["getManifest", "connectNative", "sendMessage", "openOptionsPage"]);
    if (chrome.tabs) wrap(chrome.tabs, ["get", "query"]);
    if (chrome.storage && chrome.storage.local) wrap(chrome.storage.local, ["get", "set"]);
    if (chrome.contextMenus) wrap(chrome.contextMenus, ["create", "update", "remove"]);
    if (chrome.webRequest) wrap(chrome.webRequest, ["addListener"]);
  }
  // On Chromium, chrome.* already returns Promises for these methods in MV3.
  // No shim needed.
})();
