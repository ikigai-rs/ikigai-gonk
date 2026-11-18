// cargo run --example axe_site -- target/axe-site
// deno run -A tests/browser/harness_filters_3787.mjs [target/axe-site] [screenshot directory]
// The Harness page's picker and filters (ledger #3787), from the real HTTP door's pages: each
// filter group a fieldset with a legend, each choice labeled with its count in words, the
// filtered page's sentence in #status, keyboard use, axe WCAG A/AA, and no page overflow at
// 1280, 375 and 320 px in every theme, with and without scripts.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const root = Deno.args[0] ?? "target/axe-site";
const shots = Deno.args[1];
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
  return new Response(body, { headers: { "Content-Type": types[path] } });
});
const browser = await chromium.launch();
let runs = 0;
try {
  console.log(`Chromium ${browser.version()}`);
  for (const width of [1280, 375, 320]) {
    for (const colorScheme of ["light", "dark"]) {
      for (const javaScriptEnabled of [true, false]) {
        const context = await browser.newContext({ viewport: { width, height: 900 }, colorScheme, javaScriptEnabled });
        const page = await context.newPage();
        const errors = [];
        page.on("pageerror", (error) => errors.push(String(error)));
        page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
        for (const theme of ["auto", "light", "dark", "high-contrast"]) {
          for (const file of ["harness", "harness-filtered", "harness-unchecked"]) {
            await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html?theme=${theme}`);
            // The picker: a labeled select of the repositories, one of them chosen.
            const picker = page.getByRole("combobox", { name: "Repository", exact: true });
            assert.equal(await picker.inputValue(), "fixture");
            // Each filter group is a fieldset named by its legend; each choice's name says its
            // count in words.
            const form = page.getByRole("form", { name: "Choose the rules", exact: true });
            const groups = form.getByRole("group");
            assert.ok(await groups.count() >= 7, "the filter groups");
            for (const legend of ["Category", "Lifecycle stage", "Direction", "Execution", "Firings in the log", "Coverage gap"]) {
              assert.equal(await form.getByRole("group", { name: legend, exact: true }).count(), 1, legend);
            }
            for (const box of await form.getByRole("checkbox").all()) {
              const name = await box.evaluate((node) => node.labels[0].textContent);
              assert.match(name, /\(\d+ rules?\)$/, name);
            }
            const status = page.locator("#status");
            if (file === "harness") {
              assert.equal((await status.textContent()).trim(), "");
              assert.equal(await page.getByRole("link", { name: "Clear every filter", exact: true }).count(), 0);
            } else {
              assert.match(await status.textContent(), /^\d+ of \d+ rules, filtered by: /);
              assert.equal(await page.getByRole("link", { name: "Clear every filter", exact: true }).getAttribute("href"), "/harness?root=fixture");
            }
            if (file === "harness-filtered") {
              assert.ok(await form.getByRole("checkbox", { name: /^authority\/safety \(\d+ rules?\)$/ }).isChecked());
            }
            if (file === "harness-unchecked") {
              assert.ok(await page.getByText("Unchecked because: a reviewer judges what small means", { exact: true }).isVisible());
              assert.ok(await page.getByText("No reason is stated for leaving it unchecked.").first().isVisible());
            }
            // The keyboard reaches the words, a choice and the button, and Space toggles a choice.
            await page.getByLabel("Words in the label or description", { exact: true }).focus();
            await page.keyboard.press("Tab");
            const first = form.getByRole("checkbox").first();
            assert.ok(await first.evaluate((node) => node === document.activeElement), "Tab reaches the first choice");
            const was = await first.isChecked();
            await page.keyboard.press("Space");
            assert.equal(await first.isChecked(), !was);
            const submit = form.getByRole("button", { name: "Show these rules", exact: true });
            await submit.focus();
            assert.ok(await submit.evaluate((node) => node === document.activeElement));
            if (javaScriptEnabled) {
              const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
              assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
            }
            assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no page overflow: ${file} ${width}`);
            assert.deepEqual(errors, []);
            if (shots && javaScriptEnabled && colorScheme === "light" && theme === "auto" && width !== 320) {
              await page.reload();
              await page.screenshot({ path: `${shots}/after-${file}-${width}.png`, fullPage: true });
            }
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
