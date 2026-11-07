// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/labels_3148.mjs [target/axe-site]
// The rendered ledger, filtered and empty states: axe, keyboard reachability and 320 px
// in every theme, with and without scripts. HTTP/filter behavior is in tests/web.rs.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const root = await Deno.realPath(Deno.args[0] ?? "target/axe-site");
const types = JSON.parse(await Deno.readTextFile(`${root}/_types.json`));
const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, async (request) => {
  const url = new URL(request.url);
  const path = url.pathname;
  const type = types[path];
  if (!type) return new Response("Not found", { status: 404 });
  let body = await Deno.readFile(`${root}${url.pathname}`);
  if (type.startsWith("text/html")) {
    // The fixture is rendered as auto. Mirror the server's theme attribute for no-script runs.
    const theme = url.searchParams.get("theme");
    if (["auto", "light", "dark", "high-contrast"].includes(theme)) {
      body = new TextEncoder().encode(new TextDecoder().decode(body).replace("data-theme='auto'", `data-theme='${theme}'`));
    }
  }
  return new Response(body, { headers: { "Content-Type": type } });
});
const browser = await chromium.launch();
try {
  console.log(`Chromium ${browser.version()}`);
  for (const javaScriptEnabled of [true, false]) {
    for (const width of [1280, 320]) {
      const context = await browser.newContext({ javaScriptEnabled, viewport: { width, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
      for (const [theme, colorScheme] of [["auto", "light"], ["auto", "dark"], ["light", "dark"], ["dark", "light"], ["high-contrast", "light"]]) {
        await page.emulateMedia({ colorScheme });
        for (const file of ["ledger.html", "label-filter.html", "label-empty.html"]) {
          await page.goto(`http://127.0.0.1:${server.addr.port}/${file}?theme=${theme}`);
          assert.equal(await page.locator("html").getAttribute("data-theme"), theme);
          const labels = page.getByRole("textbox", { name: "Labels (all must match)", exact: true });
          const without = page.getByRole("textbox", { name: "Without (none may match)", exact: true });
          assert.equal(await labels.getAttribute("aria-describedby"), "label-filter-help");
          assert.equal(await without.getAttribute("aria-describedby"), "label-filter-help");
          assert.equal(await labels.inputValue(), file === "label-filter.html" ? "needs-person" : file === "label-empty.html" ? "missing" : "");
          assert.equal(await without.inputValue(), file === "ledger.html" ? "" : "blocked");
          // Tab reaches both fields, their submit button, then the plain filter links.
          await page.getByRole("searchbox", { name: "Search titles", exact: true }).focus();
          await page.keyboard.press("Tab");
          assert.ok(await labels.evaluate((node) => node === document.activeElement));
          await page.keyboard.press("Tab");
          assert.ok(await without.evaluate((node) => node === document.activeElement));
          await page.keyboard.press("Tab");
          assert.ok(await page.getByRole("button", { name: "Search", exact: true }).evaluate((node) => node === document.activeElement));
          if (file !== "ledger.html") {
            await page.keyboard.press("Tab");
            assert.ok(await page.getByRole("link", { name: "Clear label filters", exact: true }).evaluate((node) => node === document.activeElement));
            assert.equal(await page.getByRole("link", { name: "Exclude blocked", exact: true }).count(), 0);
            assert.equal(await page.getByRole("link", { name: "Require blocked", exact: true }).count(), 1);
          }
          if (file === "label-filter.html") {
            assert.equal(await page.locator(".active-filters, .label-actions").getByRole("link", { name: "Exclude needs-person", exact: true }).count(), 2);
            assert.equal(await page.locator(".label-actions a.badge").count(), 2);
          }
          if (file === "label-empty.html") assert.ok(await page.getByText("No items match.", { exact: true }).isVisible());
          assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${file}: no page overflow`);
          if (javaScriptEnabled) {
            const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
            assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
          }
          assert.deepEqual(errors, []);
          console.log(`ok ${file} JS=${javaScriptEnabled} width=${width} theme=${theme} system=${colorScheme}`);
        }
      }
      await context.close();
    }
  }
} finally {
  await browser.close();
  await server.shutdown();
}
