// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/projects_3191.mjs
// Local static fixtures from the real HTTP door; no network publication (ledger #3191).
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
          for (const file of ["project-ledger", "project-filter", "project-empty"]) {
            await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html?theme=${theme}`);
            const empty = file === "project-empty";
            assert.equal(await page.getByRole("heading", { level: 1 }).innerText(), empty ? "Ledgers: none readable" : "Ledgers crew, default");
            const notes = await page.locator(".ledger-head .union").allInnerTexts();
            assert.match(notes[0], /^Projects: /);
            assert.match(notes[1], empty ? /^None of the ledgers asked for/ : /^Items from 2 ledgers: crew, default\./);
            const numbers = await page.locator("li.row .num").allInnerTexts();
            assert.equal(numbers.length > 0, !empty);
            for (const number of numbers) assert.match(number, /^(default|crew)#\d+$/);
            const selection = empty ? "empty" : file === "project-filter" ? "work,overlap" : "work";
            for (const status of ["open", "closed", "all"]) {
              const tab = page.getByRole("navigation", { name: "Status" }).getByRole("link", { name: status, exact: true });
              assert.match(await tab.getAttribute("href"), /projects=/);
            }
            assert.equal(await page.locator("form.search input[name=projects]").getAttribute("value"), selection);
            if (file === "project-ledger") assert.equal(await page.locator("form.search input[name=ledgers]").getAttribute("value"), "default");
            const toggle = javaScriptEnabled ? page.locator("#ledger-toggle") : page.locator("summary.ledger-summary");
            await toggle.focus();
            await page.keyboard.press("Enter");
            assert.ok(await page.getByRole("link", { name: "Project: work", exact: true }).isVisible());
            assert.match(await page.getByRole("link", { name: "Project: work", exact: true }).getAttribute("href"), /projects=work$/);
            assert.equal(await page.locator("[data-ledger-project] [data-ledger-name=private]").count(), 0);
            if (javaScriptEnabled) {
              await page.keyboard.press("Tab");
              assert.ok(await page.locator("#ledger-filter").evaluate((node) => node === document.activeElement));
              await page.locator("#ledger-filter").fill("crew");
              await page.waitForTimeout(350);
              assert.equal(await page.locator("#ledger-filter-status").innerText(), "1 of 9 ledger matches.");
              assert.ok(await page.getByRole("link", { name: "Project: work", exact: true }).isVisible());
              assert.ok(await page.getByRole("link", { name: "Project: overlap", exact: true }).isVisible());
              assert.equal(await page.getByRole("link", { name: "Project: catalog", exact: true }).isVisible(), false);
              await page.locator("#ledger-filter").fill("no-match");
              assert.equal(await page.locator("[data-ledger-project]:visible").count(), 0);
              await page.locator("#ledger-filter").fill("");
              const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
              assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
              await page.keyboard.press("Escape");
              assert.equal(await toggle.getAttribute("aria-expanded"), "false");
              assert.ok(await toggle.evaluate((node) => node === document.activeElement));
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
