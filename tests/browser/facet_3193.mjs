// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/facet_3193.mjs [target/axe-site]
// Actual rendered pages and fragments, with delayed fragment delivery: independent load,
// focus, native links, failures, no scripts, every theme and phone width. HTTP semantics
// and grants are covered in tests/web.rs.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const root = await Deno.realPath(Deno.args[0] ?? "target/axe-site");
const types = JSON.parse(await Deno.readTextFile(`${root}/_types.json`));
let fail = false;
const server = Deno.serve({ hostname: "127.0.0.1", port: 0, onListen() {} }, async (request) => {
  const url = new URL(request.url);
  let path = url.pathname;
  if (path === "/l/default/facet") {
    await new Promise((resolve) => setTimeout(resolve, 300));
    if (fail) return new Response("Unavailable", { status: 503 });
    path = url.searchParams.get("page") === "true" ? "/facet-page.html"
      : url.searchParams.get("labels") === "missing" ? "/facet-empty.fragment" : "/facet.fragment";
  }
  const type = types[path];
  if (!type) return new Response("Not found", { status: 404 });
  let body = await Deno.readFile(`${root}${path}`);
  if (type.startsWith("text/html")) {
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
        for (const file of ["ledger.html", "label-filter.html", "label-empty.html", "facet-page.html"]) {
          await page.goto(`http://127.0.0.1:${server.addr.port}/${file}?theme=${theme}`, { waitUntil: "domcontentloaded" });
          assert.equal(await page.locator("html").getAttribute("data-theme"), theme);
          if (file !== "facet-page.html") {
            const search = page.getByRole("searchbox", { name: "Search titles", exact: true });
            await search.focus();
            if (javaScriptEnabled) {
              // The list is already visible while its counts are still in flight.
              assert.ok(await page.locator("#ledger").isVisible());
              await page.waitForSelector("#listing-facet[data-loaded]");
              assert.ok(await search.evaluate((node) => node === document.activeElement), "arrival keeps focus");
              assert.ok(await page.locator("#status").textContent().then((s) => s.includes("Labels and priority counts loaded.")));
              if (file === "label-empty.html") {
                assert.ok(await page.getByText("No labels or priorities match.", { exact: true }).isVisible());
              } else {
                const include = page.locator("#listing-facet a").first();
                await include.focus();
                assert.ok(await include.evaluate((node) => node === document.activeElement));
                assert.ok((await include.getAttribute("href")).includes("labels="));
                await page.keyboard.press("Tab");
                assert.ok(await page.locator("#listing-facet a").nth(1).evaluate((node) => node === document.activeElement));
                assert.match(await page.locator("#listing-facet a").nth(1).textContent(), /^Exclude /);
              }
            } else {
              assert.equal(await page.locator("#listing-facet[data-loaded]").count(), 0);
              const fallback = page.getByRole("link", { name: "Open labels and priority counts", exact: true });
              assert.ok(await fallback.isVisible());
              assert.ok((await fallback.getAttribute("href")).includes("page=true"));
            }
          }
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
  // Failure stays beside the counts and is announced once, without clearing the list.
  fail = true;
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.addr.port}/ledger.html`);
  await page.waitForFunction(() => document.querySelector("#listing-facet")?.textContent.includes("Counts could not load"));
  assert.ok(await page.locator(".items").isVisible());
  assert.ok((await page.locator("#status").textContent()).includes("Counts could not load"));
  assert.ok(await page.getByRole("link", { name: "Open labels and priority counts", exact: true }).isVisible());
  console.log("ok failed independent load");
} finally {
  await browser.close();
  await server.shutdown();
}
