// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/dashboard_tabs_2971.mjs [target/debug/ikigai-gonk]
//
// Ledger #2971, over a scratch gonk and a virtual passkey, in real time (the polls are every 30 s, so
// a run takes some minutes): the ARIA tabs pattern and its keys; only the shown panel polls, and a
// switch fetches the new one once; Back and Forward move between tabs; a pause holds whichever
// panel is shown; a hidden browser tab asks nothing (`document.hidden` overridden, as headless never
// hides a tab); a failed switch says so; with no script the tabs are plain links; axe on both
// panels in every theme at 1280 and 320 px. Exits non-zero on any failure.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-dashboard-tabs-" });
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(`${scratch}/config/config.toml`, `gonk.port = ${port}\n`);
const homes = ["--config-home", `${scratch}/config`, "--data-home", `${scratch}/data`];
const server = new Deno.Command(binary, {
  args: [...homes, "--no-quic", "--no-backup"],
  stdout: "null",
  stderr: "null",
}).spawn();

const POLL_MS = 30_000;
const SLACK_MS = 6_000;
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

const browser = await chromium.launch();
const failures = [];
const check = (ok, said) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${said}`);
  if (!ok) failures.push(said);
};
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
  const invited = await new Deno.Command(binary, {
    args: ["passkey", "invite", "keyboard", "--ledger", "default=delete", ...homes],
  }).output();
  const said = new TextDecoder().decode(invited.stdout);
  assert.ok(invited.success, said + new TextDecoder().decode(invited.stderr));
  const invite = said.match(/http:\/\/\S*#invite=\S+/)[0];

  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error" && !/status of (304|403)|ERR_FAILED/.test(message.text())) {
      errors.push(message.text());
    }
  });
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

  const writer = await context.newPage();
  const act = async (fields) => {
    await writer.goto(`${base}/l/default`);
    const status = await writer.evaluate(async (fields) => {
      const response = await fetch("/act", {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams(fields).toString(),
      });
      return response.status;
    }, fields);
    assert.ok(status < 400, `the write answered ${status}: ${JSON.stringify(fields)}`);
  };
  await act({ _ledger: "default", _action: "append", content: "first sample" });
  await act({ _ledger: "default", _action: "append", content: "second sample" });
  await act({ _ledger: "default", _action: "claim", item: "1", content: "sample-agent" });

  // Every ask of a fragment the page makes.
  const asks = [];
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (/^\/dashboard\/.+/.test(path) && request.resourceType() !== "document") asks.push(path);
  });
  const since = (from) => asks.slice(from);
  const isPanel = (path) => path.startsWith("/dashboard/panel/");
  const isLedgers = (path) => path.startsWith("/dashboard/chart/");
  const isAgents = (path) => path.startsWith("/dashboard/agents");

  const tabs = page.getByRole("tablist", { name: "Dashboard views" });
  const ledgersTab = page.getByRole("tab", { name: "Ledgers" });
  const agentsTab = page.getByRole("tab", { name: "Agents" });
  const ledgersPanel = page.locator("#dashboard-panel-ledgers");
  const agentsPanel = page.locator("#dashboard-panel-agents");
  const focusId = () => page.evaluate(() => document.activeElement?.id);
  const shownReady = (id) =>
    page.locator(`#dashboard-panel-${id} [data-live][hx-get]`).first().waitFor();

  // ---- the pattern, on a load of either URL
  await page.goto(`${base}/dashboard`);
  await tabs.waitFor();
  check(await tabs.count() === 1, "one tablist, named");
  check(await ledgersTab.getAttribute("aria-selected") === "true", "/dashboard shows Ledgers");
  check(await agentsTab.getAttribute("aria-selected") === "false", "and Agents is not selected");
  check(
    await ledgersTab.getAttribute("aria-controls") === "dashboard-panel-ledgers" &&
      await agentsTab.getAttribute("aria-controls") === "dashboard-panel-agents",
    "each tab controls its panel",
  );
  check(
    await page.getByRole("tabpanel", { name: "Ledgers" }).count() === 1 &&
      await page.getByRole("tabpanel").count() === 1,
    "one tabpanel exposed, labeled by its tab",
  );
  check(await page.locator("#dashboard-tabs [aria-current]").count() === 0, "aria-current gave way");
  check(await agentsPanel.isHidden(), "the other panel is hidden");
  check(await agentsPanel.innerHTML() === "", "and empty");
  check(await page.title() === "Dashboard: Ledgers · gonk", "the title names the tab");

  // ---- the keys: one tab stop, arrows, Home and End, and focus never goes into the panel
  await page.getByRole("button", { name: "Pause updates" }).focus();
  await page.keyboard.press("Tab");
  check(await focusId() === "dashboard-tab-ledgers", "Tab from the control lands on the selected tab");
  const ring = await ledgersTab.evaluate((a) => getComputedStyle(a).outlineStyle);
  check(ring !== "none", `a keyboard's focus is visible (outline ${ring})`);
  await page.keyboard.press("Tab");
  check(
    await page.evaluate(() =>
      document.querySelector("#dashboard-panel-ledgers").contains(document.activeElement)
    ),
    "the next Tab leaves the list, into the panel: one tab stop",
  );
  await page.keyboard.press("Shift+Tab");
  check(await focusId() === "dashboard-tab-ledgers", "Shift+Tab comes back to the selected tab");

  let from = asks.length;
  await page.keyboard.press("ArrowRight");
  check(await focusId() === "dashboard-tab-agents", "ArrowRight moves to Agents");
  await shownReady("agents");
  check(await agentsTab.getAttribute("aria-selected") === "true", "and selects it");
  check(await agentsTab.getAttribute("tabindex") === "0", "it is now the tab stop");
  check(await ledgersTab.getAttribute("tabindex") === "-1", "and Ledgers is not");
  check(new URL(page.url()).pathname === "/dashboard/agents", "the URL follows");
  check(await page.title() === "Dashboard: Agents · gonk", "the title follows");
  check(await ledgersPanel.isHidden() && await ledgersPanel.innerHTML() === "", "Ledgers is emptied");
  check(await agentsPanel.getByText("sample-agent").first().isVisible(), "Agents shows its table");
  check(await focusId() === "dashboard-tab-agents", "focus stays on the tab once it has loaded");
  check(
    JSON.stringify(since(from)) === JSON.stringify(["/dashboard/panel/agents"]),
    `a switch fetches its panel once: ${since(from)}`,
  );
  await page.keyboard.press("ArrowRight");
  check(await focusId() === "dashboard-tab-ledgers", "ArrowRight wraps round to Ledgers");
  await shownReady("ledgers");
  await page.keyboard.press("ArrowLeft");
  check(await focusId() === "dashboard-tab-agents", "ArrowLeft wraps back to Agents");
  await shownReady("agents");
  await page.keyboard.press("Home");
  check(await focusId() === "dashboard-tab-ledgers", "Home goes to the first");
  await shownReady("ledgers");
  await page.keyboard.press("End");
  check(await focusId() === "dashboard-tab-agents", "End goes to the last");
  await shownReady("agents");
  const scrolled = await page.evaluate(() => window.scrollY);
  await page.keyboard.press("Space");
  check(await page.evaluate(() => window.scrollY) === scrolled, "Space on a tab does not scroll");
  check(await agentsTab.getAttribute("aria-selected") === "true", "and leaves it selected");

  // ---- only the shown panel polls: Agents now, then Ledgers
  from = asks.length;
  await sleep(POLL_MS + SLACK_MS);
  let round = since(from);
  check(
    round.length >= 2 && round.every(isAgents),
    `on Agents only its fragments are asked: ${round}`,
  );
  await ledgersTab.click();
  await shownReady("ledgers");
  check(await focusId() === "dashboard-tab-ledgers", "a click leaves focus on the tab, not the panel");
  from = asks.length;
  await sleep(POLL_MS + SLACK_MS);
  round = since(from);
  check(
    round.length >= 1 && round.every(isLedgers),
    `on Ledgers only its fragments are asked: ${round}`,
  );
  // A close, seen by the shown panel with no reload, so a switched-in panel really is live.
  await act({ _ledger: "default", _action: "close", item: "2", reason: "done" });
  await page.waitForFunction(
    () => /1 open, 1 closed/.test(document.querySelector("#chart-default-summary")?.textContent),
    null,
    { timeout: POLL_MS + SLACK_MS },
  );
  check(true, "a panel fetched by a switch goes on polling");

  // ---- history: Back and Forward, and a reload
  await page.goto(`${base}/dashboard/ledgers`);
  await tabs.waitFor();
  await agentsTab.click();
  await shownReady("agents");
  await page.goBack();
  await shownReady("ledgers");
  check(await ledgersTab.getAttribute("aria-selected") === "true", "Back returns to Ledgers");
  check(new URL(page.url()).pathname === "/dashboard/ledgers", "at its URL");
  check(await ledgersPanel.locator("figure").first().isVisible(), "with its chart");
  check((await page.locator("#status").textContent()).includes("Ledgers shown."), "and says which");
  await page.goForward();
  await shownReady("agents");
  check(await agentsTab.getAttribute("aria-selected") === "true", "Forward goes to Agents again");
  await page.reload();
  await shownReady("agents");
  check(await agentsTab.getAttribute("aria-selected") === "true", "a reload keeps the tab");
  check(await ledgersPanel.innerHTML() === "", "with the other panel empty");
  // Back with focus on a link in the panel going away: the tab has it, not <body>.
  await ledgersTab.click();
  await shownReady("ledgers");
  await ledgersPanel.locator("a").first().focus();
  await page.goBack();
  await shownReady("agents");
  check(await focusId() === "dashboard-tab-agents", "Back from inside a panel leaves focus on the tab");
  // Two switches before the first answers: the last one wins.
  await page.route("**/dashboard/panel/ledgers", async (route) => {
    await sleep(1_500);
    await route.continue().catch(() => {});
  });
  await ledgersTab.click();
  check(
    await ledgersPanel.getByText("Loading…").isVisible(),
    "a slow panel says it is loading",
  );
  await agentsTab.click();
  await shownReady("agents");
  await sleep(2_000);
  check(await agentsTab.getAttribute("aria-selected") === "true", "a late answer does not take over");
  check(await ledgersPanel.innerHTML() === "", "and is not drawn");
  await page.unroute("**/dashboard/panel/ledgers");

  // ---- the pause applies to whichever panel is shown
  const pause = page.getByRole("button", { name: "Pause updates" });
  const resume = page.getByRole("button", { name: "Resume updates" });
  await pause.click();
  from = asks.length;
  await ledgersTab.click();
  await shownReady("ledgers");
  check(
    JSON.stringify(since(from)) === JSON.stringify(["/dashboard/panel/ledgers"]),
    "paused, a switch still fetches the panel a person asked to see",
  );
  await sleep(POLL_MS + SLACK_MS);
  check(since(from).length === 1, `and then asks nothing: ${since(from)}`);
  check(await resume.isVisible(), "the control still says it is paused");
  from = asks.length;
  await resume.click();
  await sleep(SLACK_MS);
  check(
    since(from).length >= 1 && since(from).every(isLedgers),
    `resuming asks at once, for the shown panel: ${since(from)}`,
  );

  // ---- a hidden browser tab asks nothing, on either panel
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  from = asks.length;
  await sleep(POLL_MS + SLACK_MS);
  check(since(from).length === 0, "a hidden browser tab asks nothing");
  await page.evaluate(() => {
    delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await sleep(SLACK_MS);
  check(
    since(from).length >= 1 && since(from).every(isLedgers),
    `showing it asks for the shown panel only: ${since(from)}`,
  );

  // ---- a failed switch says so, with the way out
  await page.route("**/dashboard/panel/agents", (route) => route.abort());
  await agentsTab.click();
  const again = agentsPanel.getByRole("link", { name: "Load it again" });
  await again.waitFor();
  check(await again.getAttribute("href") === "/dashboard/agents", "a failed switch links its page");
  check(
    (await page.locator("#status").textContent()).includes("did not load"),
    "and says so in the status region",
  );
  check(await focusId() === "dashboard-tab-agents", "focus stays on the tab");
  await page.unroute("**/dashboard/panel/agents");
  await again.click();
  await shownReady("agents");
  check(true, "and the link loads it as a page");

  // ---- a modified click is the link's own: a new tab, this page unchanged
  await page.goto(`${base}/dashboard/ledgers`);
  await tabs.waitFor();
  const [opened] = await Promise.all([
    context.waitForEvent("page"),
    agentsTab.click({ modifiers: ["Control"] }),
  ]);
  await opened.waitForURL("**/dashboard/agents", { timeout: 5_000 });
  check(true, "Ctrl+click opens the tab's URL");
  check(await ledgersTab.getAttribute("aria-selected") === "true", "and this page stays put");
  await opened.close();

  // ---- axe and the width, on both panels, in each theme
  for (const path of ["ledgers", "agents"]) {
    for (const theme of ["auto", "light", "dark", "high-contrast"]) {
      for (const width of [1280, 320]) {
        await page.setViewportSize({ width, height: 800 });
        await page.goto(`${base}/dashboard/${path}?theme=${theme}`);
        await tabs.waitFor();
        const axe = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
          .analyze();
        check(
          axe.violations.length === 0,
          `axe, ${path}, ${theme} at ${width}: ${
            axe.violations.map((v) => v.id).join(", ") || "no violation"
          }`,
        );
        const wide = await page.evaluate(() => document.documentElement.scrollWidth);
        check(wide <= width, `${path}, ${theme} at ${width}: scrollWidth ${wide}`);
      }
    }
  }
  // Axe on a panel a switch brought, not only one a load drew.
  await page.setViewportSize({ width: 1280, height: 800 });
  await page.goto(`${base}/dashboard/ledgers?theme=dark`);
  await tabs.waitFor();
  await agentsTab.click();
  await shownReady("agents");
  const switched = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  check(
    switched.violations.length === 0,
    `axe after a switch: ${switched.violations.map((v) => v.id).join(", ") || "no violation"}`,
  );

  // ---- with no script: plain links, each a whole page
  const off = await browser.newContext({ javaScriptEnabled: false });
  const still = await off.newPage();
  await still.goto(`${base}/dashboard`);
  check(await still.locator("[role=tablist], [role=tab], [role=tabpanel]").count() === 0, "no script, no ARIA tabs");
  check(
    await still.locator('#dashboard-tab-ledgers[aria-current="page"]').count() === 1,
    "the current tab is a link that says so",
  );
  check(await still.locator("#dashboard-panel-ledgers figure").first().isVisible(), "with its chart");
  await still.getByRole("link", { name: "Agents", exact: true }).click();
  await still.waitForURL("**/dashboard/agents");
  check(true, "the other is a plain link to its own URL");
  check(
    await still.locator('#dashboard-tab-agents[aria-current="page"]').count() === 1,
    "which selects that tab",
  );
  check(await still.locator("section.agent-panel").count() >= 1, "and loads its panel as a full page");
  await still.goBack();
  await still.waitForURL("**/dashboard");
  check(await still.locator("#dashboard-panel-ledgers figure").count() >= 1, "and Back goes back");
  await off.close();

  check(errors.length === 0, `no page or console error: ${errors.join(" | ")}`);
  console.log(`Chromium ${browser.version()}`);
} finally {
  await browser.close();
  server.kill("SIGTERM");
  await server.status;
  await Deno.remove(scratch, { recursive: true }).catch(() => {});
}
if (failures.length) {
  console.error(`${failures.length} failed:\n${failures.join("\n")}`);
  Deno.exit(1);
}
