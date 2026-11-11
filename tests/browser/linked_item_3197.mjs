// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/linked_item_3197.mjs
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
          await page.goto(`http://127.0.0.1:${server.addr.port}/linked-item.html`);
          // Playwright evaluation also works when the page's own scripts are disabled.
          await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
          const panel = page.getByRole("region", { name: "Links", exact: true });
          assert.equal(await panel.locator("li.row").count(), 4);
          assert.equal(await page.getByRole("heading", { level: 1 }).innerText(), "A live flight plan");
          assert.equal(await panel.getByRole("heading", { name: "related · default", exact: true }).count(), 1);
          assert.equal(await panel.getByRole("heading", { name: "related · crew", exact: true }).count(), 1);
          assert.equal(await panel.getByRole("link", { name: "A crew item (in crew)", exact: true }).count(), 1);
          assert.match(await panel.innerText(), /filed: 1; resolving: 1/);
          assert.match(await panel.innerText(), /lease expired/);
          assert.match(await panel.innerText(), /awaiting dispatcher/);
          const privateIri = panel.locator("code.iri", { hasText: "urn:iki:ledger:private:item:01unreadable" });
          assert.equal(await privateIri.count(), 1);
          assert.equal(await panel.locator("a[href*='/l/private/']").count(), 0);
          assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no page overflow");
          // Reach each member title using the keyboard, including the other ledger.
          const visited = new Set();
          for (let step = 0; step < 100 && visited.size < 4; step++) {
            await page.keyboard.press("Tab");
            const href = await page.evaluate(() => document.activeElement?.matches(".links a.title") ? document.activeElement.getAttribute("href") : null);
            if (href) visited.add(href);
          }
          assert.equal(visited.size, 4, "every linked title is a keyboard stop");
          if (javaScriptEnabled) {
            const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
            assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
          }
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
