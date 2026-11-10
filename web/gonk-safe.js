// Safe mode's per-browser toggle (ledger #3379): a page with a deployment's own stylesheet or
// script links this file, WITHOUT defer, just before them. When this browser has asked for safe
// mode (web/gonk.js's "Use safe mode on every page in this browser", kept in localStorage) and
// the page was not already asked for with ?safe=1, it stops the page before the parser reaches
// the deployment's assets and loads the same address with ?safe=1, which the server draws
// without them. Nothing else: no listener, no global, and no failure that reaches the page.
(function () {
  "use strict";
  try {
    if (localStorage.getItem("gonk:safe") !== "1") return;
    const url = new URL(location.href);
    if (url.searchParams.get("safe") === "1") return;
    url.searchParams.set("safe", "1");
    window.stop();
    location.replace(url.href);
  } catch (_) {
    // No storage (a private window, a refused origin): the query flag still works.
  }
})();
