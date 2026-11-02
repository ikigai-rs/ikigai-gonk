// Run after `cargo run --example axe_site -- target/focus-site`:
// deno run -A tests/browser/focus_2902.mjs target/focus-site
import { chromium } from "npm:playwright@1.64.0";
import assert from "node:assert/strict";

const root = Deno.args[0] ?? "target/focus-site";
const shell = await Deno.readTextFile(`${root}/static/shell.js`);
const htmx = await Deno.readTextFile(`${root}/static/htmx.min.js`);
const gonk = await Deno.readTextFile(`${root}/static/gonk.js`);
const browser = await chromium.launch();
try {
  // Check the shared shell alone too: gonk must not mask a regression in its dependency.
  for (const own of [false, true]) {
    const page = await browser.newPage();
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("http://gonk.test/**", (route) => {
      const path = new URL(route.request().url()).pathname;
      const scripts = {
        "/shell.js": shell,
        "/htmx.js": htmx,
        "/gonk.js": gonk,
      };
      if (path in scripts) {
        return route.fulfill({
          contentType: "text/javascript",
          body: scripts[path],
        });
      }
      if (path === "/poll") {
        return route.fulfill({
          contentType: "text/html",
          body: "Updated count",
        });
      }
      return route.fulfill({
        contentType: "text/html",
        body: `<!doctype html>
        <html><head><script src="/shell.js"></script><script src="/htmx.js"></script>
        ${own ? '<script src="/gonk.js"></script>' : ""}</head><body>
        <div id="status" role="status"></div>
        <span id="badge" data-ui-quiet hx-get="/poll" hx-trigger="every 200ms">Count</span>
        <main id="main" style="height:4000px"><h1>Reading a ledger</h1></main>
        </body></html>`,
      });
    });
    await page.goto("http://gonk.test/");
    await page.evaluate(() => {
      window.scrollTo(0, 1000);
      window.settles = 0;
      document.addEventListener("htmx:afterSettle", () => window.settles++);
    });
    await page.waitForFunction(() => window.settles >= 2);
    const state = await page.evaluate(() => ({
      body: document.activeElement === document.body,
      scroll: window.scrollY,
      badge: document.getElementById("badge").textContent,
    }));
    assert.deepEqual(state, {
      body: true,
      scroll: 1000,
      badge: "Updated count",
    }, `idle poll with ${own ? "gonk and shell" : "shell alone"}`);
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log(
    `Idle polls preserve body focus and scroll: shell alone and gonk (${browser.version()})`,
  );
} finally {
  await browser.close();
}
