// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/inbound_3217.mjs
// Static fixtures from the real HTTP door; no network publication.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const root = Deno.args[0] ?? "target/axe-site";
const types = JSON.parse(await Deno.readTextFile(`${root}/_types.json`));
const server = Deno.serve({ port: 0, hostname: "127.0.0.1", onListen() {} }, async (request) => {
  const path = new URL(request.url).pathname;
  if (!(path in types)) return new Response("Not found", { status: 404 });
  return new Response(await Deno.readFile(`${root}${path}`), {
    headers: { "Content-Type": types[path] },
  });
});
const unreadable = "urn:iki:ledger:private:item:01unreadable";
const browser = await chromium.launch();
try {
  console.log(`Chromium ${browser.version()}`);
  for (const javaScriptEnabled of [true, false]) {
    for (const width of [1280, 320]) {
      for (const colorScheme of ["light", "dark"]) {
        for (const theme of ["auto", "light", "dark", "high-contrast"]) {
          const context = await browser.newContext({ viewport: { width, height: 900 }, colorScheme, javaScriptEnabled });
          const page = await context.newPage();
          const errors = [];
          page.on("pageerror", error => errors.push(String(error)));
          page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
          for (const path of ["/inbound-item.html", "/ledger.html"]) {
            await page.goto(`http://127.0.0.1:${server.addr.port}${path}`);
            // Playwright evaluation also works when the page's own scripts are disabled.
            await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
            // The blocked badge says its fact in words, naming each open blocker.
            const badge = page.locator(".badge.blocked").first();
            assert.match(await badge.innerText(), /^blocked by #4, crew 01[0-9a-z]+, urn:iki:ledger:private:item:01unreadable$/);
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no page overflow on ${path}`);
            if (javaScriptEnabled) {
              const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
              assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
            }
          }
          await page.goto(`http://127.0.0.1:${server.addr.port}/inbound-item.html`);
          await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
          const panel = page.getByRole("region", { name: "Linked to this", exact: true });
          assert.equal(await panel.getByRole("heading", { name: "blocks this · default", exact: true }).count(), 1);
          assert.equal(await panel.getByRole("heading", { name: "related to this · default", exact: true }).count(), 1);
          assert.equal(await panel.getByRole("heading", { name: "blocks this · crew", exact: true }).count(), 1);
          assert.equal(await panel.getByRole("link", { name: "A crew item (in crew)", exact: true }).count(), 1);
          assert.equal(await panel.locator("code.iri", { hasText: unreadable }).count(), 1);
          assert.equal(await panel.locator("a[href*='/l/private/']").count(), 0);
          // Nothing in the panel unlinks: an edge belongs to the item that holds it.
          assert.equal(await panel.getByRole("button", { name: /^Unlink/ }).count(), 0);
          // Each linker's title is a keyboard stop.
          const visited = new Set();
          for (let step = 0; step < 150 && visited.size < 2; step++) {
            await page.keyboard.press("Tab");
            const href = await page.evaluate(() => document.activeElement?.matches(".inbound a.title") ? document.activeElement.getAttribute("href") : null);
            if (href) visited.add(href);
          }
          assert.equal(visited.size, 2, "every readable linker's title is a keyboard stop");
          assert.deepEqual(errors, []);
          console.log(`ok script=${javaScriptEnabled} width=${width} system=${colorScheme} theme=${theme}`);
          await context.close();
        }
      }
    }
  }
} finally {
  await browser.close();
  await server.shutdown();
}
