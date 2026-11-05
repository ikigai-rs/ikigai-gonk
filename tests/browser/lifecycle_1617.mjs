// After cargo build: deno run -A tests/browser/lifecycle_1617.mjs [target/debug/ikigai-gonk]
// A scratch server: the doctor's link, lifecycle semantics, keyboard access to the scrolling
// table, 320 px overflow, and axe in every theme, with and without JavaScript.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const scratch = await Deno.makeTempDir({ dir: ".", prefix: ".lifecycle-1617-" });
const port = 20000 + Math.floor(Math.random() * 20000);
const server = new Deno.Command(binary, {
  args: ["--config-home", `${scratch}/config`, "--data-home", `${scratch}/data`,
    "--port", String(port), "--no-quic", "--no-backup"],
  stdout: "null", stderr: "null",
}).spawn();
const browser = await chromium.launch();
try {
  const base = `http://localhost:${port}`;
  for (let tries = 0;; tries++) {
    try {
      await (await fetch(`${base}/`)).body?.cancel();
      break;
    } catch (error) {
      if (tries >= 100) throw error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  console.log(`Chromium ${browser.version()}`);
  for (const javaScriptEnabled of [true, false]) {
    for (const width of [1280, 320]) {
      const context = await browser.newContext({ javaScriptEnabled, viewport: { width, height: 900 } });
      const page = await context.newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      page.on("console", (message) => { if (message.type() === "error") errors.push(message.text()); });
      for (const theme of ["auto", "light", "dark", "high-contrast"]) {
        await page.goto(`${base}/l/default/doctor?theme=${theme}`);
        const link = page.getByRole("link", { name: "kata-flight lifecycle", exact: true });
        assert.equal(await link.getAttribute("href"), "/l/default/lifecycle");
        await link.click();
        await page.goto(`${base}/l/default/lifecycle?theme=${theme}`);
        await page.getByRole("heading", { level: 1, name: "kata-flight lifecycle" }).waitFor();
        assert.deepEqual(await page.locator("tbody th").allTextContents(),
          ["filed", "queued", "reviewed", "resolving", "refining", "shipping"]);
        const region = page.getByRole("region", { name: "States in lifecycle order" });
        await page.locator("body").click({ position: { x: 1, y: 1 } });
        let reached = false;
        for (let tabs = 0; tabs < 40; tabs++) {
          await page.keyboard.press("Tab");
          if (await region.evaluate((node) => node === document.activeElement)) { reached = true; break; }
        }
        assert.ok(reached, "Tab reaches the table's scrolling region");
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), "no page overflow");
        // Axe needs page scripts enabled. The no-script runs cover navigation, semantics,
        // keyboard access and overflow; the scripted runs additionally evaluate axe.
        if (javaScriptEnabled) {
          const result = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
          assert.deepEqual(result.violations, [], JSON.stringify(result.violations));
        }
        assert.deepEqual(errors, []);
        console.log(`ok JS=${javaScriptEnabled} width=${width} theme=${theme}`);
      }
      await context.close();
    }
  }
} finally {
  await browser.close();
  server.kill("SIGTERM");
  await server.status;
  await Deno.remove(scratch, { recursive: true });
}
