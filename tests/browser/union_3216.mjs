// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/union_3216.mjs
// Local static fixtures from the real HTTP door; no network publication (ledger #3216).
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
let runs = 0;
try {
  console.log(`Chromium ${browser.version()}`);
  for (const width of [1280, 320]) {
    for (const colorScheme of ["light", "dark"]) {
      for (const javaScriptEnabled of [true, false]) {
        const context = await browser.newContext({ viewport: { width, height: 900 }, colorScheme, javaScriptEnabled });
        const page = await context.newPage();
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
        for (const theme of ["auto", "light", "dark", "high-contrast"]) {
          for (const file of ["union-ledger", "union-filter"]) {
            await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html`);
            if (javaScriptEnabled) {
              await page.evaluate((theme) => document.documentElement.dataset.theme = theme, theme);
            }
            // The heading and the sentence under it say what the page holds, in words.
            assert.equal(await page.getByRole("heading", { level: 1 }).innerText(), "Ledgers crew, default");
            const note = await page.locator(".ledger-head .union").innerText();
            assert.match(note, /^Items from 2 ledgers: crew, default\./);
            if (file === "union-ledger") {
              assert.match(note, /Left out, because this grant cannot read it: private\.$/);
            }
            // Every row's number carries its ledger; its link names it and goes to it.
            const numbers = await page.locator("li.row .num").allInnerTexts();
            assert.ok(numbers.length > 0);
            for (const number of numbers) assert.match(number, /^(default|crew)#\d+$/);
            const crew = page.getByRole("link", { name: "A crew item (in crew)", exact: true });
            if (file === "union-ledger") assert.match(await crew.getAttribute("href"), /^\/l\/crew\/item\//);
            // Status tabs and label links keep the union.
            for (const status of ["open", "closed", "all"]) {
              const tab = page.getByRole("navigation", { name: "Status" }).getByRole("link", { name: status, exact: true });
              assert.match(await tab.getAttribute("href"), /ledgers=default%2Ccrew/);
            }
            assert.equal(await page.locator("form.search input[name=ledgers]").getAttribute("value"), file === "union-ledger" ? "default,crew,private" : "default,crew");
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no page overflow");
            if (javaScriptEnabled) {
              const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
              assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
            }
            assert.deepEqual(errors, []);
            runs++;
            console.log(`ok ${file} width=${width} system=${colorScheme} js=${javaScriptEnabled} theme=${theme}`);
          }
        }
        await context.close();
      }
    }
  }
  console.log(`${runs} runs`);
} finally {
  await browser.close();
  await server.shutdown();
}
