// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/display_rules_3196.mjs
// Local static fixtures from the real HTTP door; no network publication (ledger #3196).
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const root = Deno.args[0] ?? "target/axe-site";
const types = JSON.parse(await Deno.readTextFile(`${root}/_types.json`));
const server = Deno.serve({ port: 0, hostname: "127.0.0.1", onListen() {} }, async (request) => {
  const url = new URL(request.url);
  const path = url.pathname;
  if (!(path in types)) return new Response("Not found", { status: 404 });
  let body = await Deno.readFile(`${root}${path}`);
  if (types[path].startsWith("text/html")) {
    const theme = url.searchParams.get("theme") ?? "auto";
    assert.ok(["auto", "light", "dark", "high-contrast"].includes(theme));
    const html = new TextDecoder().decode(body).replace(/<html([^>]*)>/, (_, attrs) =>
      `<html${attrs.replace(/\sdata-theme=(["']).*?\1/g, "")} data-theme="${theme}">`);
    body = new TextEncoder().encode(html);
  }
  return new Response(body, {
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
          for (const file of ["ledger", "item", "display-group"]) {
            await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html?theme=${theme}`);
            assert.ok(await page.locator(".display-strong").count() > 0);
            assert.ok(await page.locator(".display-outline").count() > 0);
            if (file !== "item") {
              const groups = page.getByRole("region", { name: "Label groups", exact: true });
              assert.ok(await groups.isVisible());
              assert.ok(await groups.getByRole("heading", { name: "Closed groups", exact: true }).isVisible());
              const link = groups.getByRole("link").first();
              await link.focus();
              assert.ok(await link.evaluate(node => node === document.activeElement));
            }
            if (javaScriptEnabled) {
              const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
              assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
            }
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no page overflow");
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
