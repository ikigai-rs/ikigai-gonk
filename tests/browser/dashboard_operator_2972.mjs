// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/dashboard_operator_2972.mjs [target/debug/ikigai-gonk]
//
// Ledger #2972, over a scratch gonk and a virtual passkey whose grant holds the operator's two
// tokens: all four tabs and the arrow keys across them; a switch fetches the new panel once, and the
// tab left asks nothing; every number is text; axe on both panels, in the light and dark themes and
// high contrast, at 1280 and 320 px; and a passkey with no such grant sees neither tab. Exits non-zero
// on any failure.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-dashboard-operator-" });
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(`${scratch}/config/config.toml`, `gonk.port = ${port}\n`);
const homes = ["--config-home", `${scratch}/config`, "--data-home", `${scratch}/data`];
const server = new Deno.Command(binary, {
  args: [...homes, "--no-quic", "--no-backup"],
  stdout: "null",
  stderr: "null",
}).spawn();

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const browser = await chromium.launch();
const failures = [];
const check = (ok, said) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${said}`);
  if (!ok) failures.push(said);
};

// A browser context signed in on `invite`, over Chromium's virtual authenticator.
async function signedIn(invite) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  await page.goto(invite);
  await page.fill("#enrol-label", "keyboard");
  await page.locator("#enrol-form button[type=submit]").click();
  await page.locator("#status .flash.ok").waitFor();
  await page.locator("#auth-login").click();
  await page.locator("#auth-who").waitFor();
  return { context, page };
}

async function inviteFor(name, extra) {
  const invited = await new Deno.Command(binary, {
    args: ["passkey", "invite", name, "--ledger", "default=delete", ...homes],
  }).output();
  const said = new TextDecoder().decode(invited.stdout);
  assert.ok(invited.success, said + new TextDecoder().decode(invited.stderr));
  if (extra.length) {
    const path = `${scratch}/config/gonk/grants.json`;
    const grants = JSON.parse(await Deno.readTextFile(path));
    const holder = grants.grants ?? grants;
    holder[name] = [...holder[name], ...extra];
    await Deno.writeTextFile(path, JSON.stringify(grants, null, 2));
  }
  return said.match(/http:\/\/\S*#invite=\S+/)[0];
}

try {
  const base = `http://localhost:${port}`;
  for (let tries = 0;; tries++) {
    try {
      await (await fetch(`${base}/`)).body?.cancel();
      break;
    } catch (error) {
      if (tries >= 100) throw error;
      await sleep(100);
    }
  }

  const operator = await signedIn(await inviteFor("operator", [
    "urn:cap:kernel:inspect",
    "urn:cap:gonk:system:read",
  ]));
  const page = operator.page;
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));

  await page.goto(`${base}/dashboard/cache`);
  const tabs = page.locator('#dashboard-tabs [role="tab"]');
  check(await tabs.count() === 4, "an operator has four tabs");
  check(
    await page.locator("#dashboard-tab-cache").getAttribute("aria-selected") === "true",
    "/dashboard/cache selects the Cache tab",
  );
  check(await page.locator('[role="tabpanel"]:not([hidden]) h3').count() === 3, "the Cache panel has its three parts");

  // A switch asks for the new panel once, and nothing of the panel left.
  const asked = [];
  page.on("request", (r) => {
    const path = new URL(r.url()).pathname;
    if (path.startsWith("/dashboard/")) asked.push(path);
  });
  await page.locator("#dashboard-tab-cache").focus();
  await page.keyboard.press("ArrowRight");
  await page.locator("#dashboard-panel-system h3").first().waitFor();
  check(await page.evaluate(() => document.activeElement.id) === "dashboard-tab-system", "the arrow key moves to the System tab, and focus stays on a tab");
  check(page.url().endsWith("/dashboard/system"), "the URL follows");
  check(asked.filter((p) => p === "/dashboard/panel/system").length === 1, "one fetch of the panel");
  check(!asked.some((p) => /cache|entries|threads|uncached/.test(p)), "the Cache tab left asks nothing");
  check(await page.locator("#dashboard-panel-cache").innerHTML() === "", "the panel left is empty");
  const text = await page.locator("#dashboard-panel-system").innerText();
  for (const said of ["Disk", "Free", "Store", "Quads", "Last backup", "Commit", "Doors listening", "Workers"]) {
    check(text.includes(said), `the System panel says ${said}`);
  }
  check(await page.locator("#dashboard-panel-system .needs-attention strong").first().innerText() === "Needs attention:", "a bad row says so in words");

  // axe, on each panel, in every theme, at both widths.
  const themes = ["auto", "light", "dark", "high-contrast"];
  for (const path of ["/dashboard/cache", "/dashboard/system"]) {
    for (const [width, height] of [[1280, 900], [320, 700]]) {
      await page.setViewportSize({ width, height });
      for (const theme of themes) {
        await page.goto(`${base}${path}?theme=${theme}`);
        const { violations } = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
          .analyze();
        check(
          violations.length === 0,
          `axe ${path} ${theme} at ${width} px: ${violations.map((v) => `${v.id} (${v.nodes[0].target})`).join(", ") || "clean"}`,
        );
        const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
        check(!overflow, `${path} ${theme} at ${width} px does not scroll the page sideways`);
      }
    }
  }
  await page.setViewportSize({ width: 1280, height: 900 });

  // With no script the tabs are the plain links, and the panel the server drew.
  const plain = await browser.newContext({ javaScriptEnabled: false, storageState: await operator.context.storageState() });
  const still = await plain.newPage();
  await still.goto(`${base}/dashboard/system`);
  check(await still.locator("#dashboard-panel-system h3").count() === 5, "with no script the page draws the System panel");
  check(await still.locator('#dashboard-tab-system[aria-current="page"]').count() === 1, "and its link says it is current");
  await plain.close();

  check(errors.length === 0, `no page errors: ${errors.join("; ")}`);

  // A passkey with no such grant sees neither.
  const viewer = await signedIn(await inviteFor("viewer", []));
  await viewer.page.goto(`${base}/dashboard/ledgers`);
  check(await viewer.page.locator("#dashboard-tab-cache").count() === 0, "no Cache tab for a viewer");
  check(await viewer.page.locator("#dashboard-tab-system").count() === 0, "no System tab for a viewer");
  const refused = await viewer.page.goto(`${base}/dashboard/system`);
  check(refused.status() === 403, "its URL answers 403");
} finally {
  await browser.close();
  try { server.kill(); } catch (_) { /* it already stopped, and the run says why */ }
  await server.status;
  await Deno.remove(scratch, { recursive: true }).catch(() => {});
}
console.log(failures.length ? `${failures.length} FAILED` : "all passed");
Deno.exit(failures.length ? 1 : 0);
