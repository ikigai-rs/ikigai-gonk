// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/row_numbers_3215.mjs
// Local static fixtures from the real HTTP door; no network publication.
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
  for (const width of [1280, 320]) {
    for (const colorScheme of ["light", "dark"]) {
      const context = await browser.newContext({ viewport: { width, height: 900 }, colorScheme });
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
      for (const theme of ["auto", "light", "dark", "high-contrast"]) {
        for (const file of ["ledger", "item", "named-ledger", "named-item", "dashboard"]) {
          await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html`);
          await page.evaluate((theme) => document.documentElement.dataset.theme = theme, theme);
          if (file.endsWith("ledger")) {
            assert.equal(await page.locator("li.row .num").innerText(), "#1");
            const ledger = file === "named-ledger" ? "crew" : "default";
            const title = ledger === "crew" ? "A crew item" : "An item for the checker to read";
            const link = page.getByRole("link", { name: `${title} (in ${ledger})`, exact: true });
            assert.match(await link.getAttribute("href"), new RegExp(`^/l/${ledger}/item/`));
            assert.equal(await link.evaluate((node) => node.firstChild.textContent), title);
          }
          if (file.endsWith("item")) {
            assert.equal(await page.locator(".item-head .num").innerText(), "#1");
          }
          assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no page overflow");
          const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
          assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
          assert.deepEqual(errors, []);
          console.log(`ok ${file} width=${width} system=${colorScheme} theme=${theme}`);
        }
      }
      await context.close();
    }
  }
} finally {
  await browser.close();
  await server.shutdown();
}
