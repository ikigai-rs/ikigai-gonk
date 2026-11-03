// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/dashboard_poll_2967.mjs [target/debug/ikigai-gonk]
//
// Ledger #2967, over a scratch gonk and a virtual passkey, in real time (the polls are every 30 s,
// so a run takes some minutes): a close updates the chart with no reload and focus stays put; an
// unchanged fragment answers 304 with nothing touched; an update is one polite sentence; an open
// data table stays open; a panel holding focus waits; a pause asks nothing and survives a reload;
// a hidden tab asks nothing (`document.hidden` overridden, as headless never hides a tab); a table
// whose links alone changed is left alone; no animation runs; the 320 px page does not scroll sideways. Exits
// non-zero on any failure.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-dashboard-poll-" });
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(
  `${scratch}/config/config.toml`,
  `gonk.port = ${port}\n`,
);
const homes = [
  "--config-home",
  `${scratch}/config`,
  "--data-home",
  `${scratch}/data`,
];
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

  const context = await browser.newContext({
    viewport: { width: 1280, height: 900 },
    reducedMotion: "reduce",
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error" && !/status of (304|403)/.test(message.text())) {
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

  // A second tab in the same sign-in, for the writes the Dashboard should see.
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
  await act({ _ledger: "default", _action: "append", content: "third sample" });

  // Every poll the page makes, with its answer's status and body size.
  const polls = [];
  page.on("response", async (response) => {
    const url = new URL(response.url());
    if (!url.pathname.startsWith("/dashboard/")) return;
    const sent = response.request().headers()["if-none-match"];
    let bytes = 0;
    try {
      bytes = (await response.body()).length;
    } catch (_) {
      // A 304 has no body to fetch.
    }
    polls.push({ path: url.pathname, status: response.status(), sent: !!sent, bytes });
  });
  const askedSince = (from) => polls.length - from;

  await page.goto(`${base}/dashboard`);
  const summary = page.locator("#chart-default-summary");
  const pause = page.getByRole("button", { name: "Pause updates" });
  const chart = () => summary.textContent();
  const status = () => page.locator("#status").textContent();
  await pause.waitFor();
  check(/3 open, 0 closed/.test(await chart()), "the chart starts at three open");
  check(
    /update every 30 seconds/.test(await page.locator("#dashboard-live-state").textContent()),
    "the sentence says the cadence once the script runs",
  );

  // An open table, and focus on something outside every fragment.
  await page.locator("details.chart-data summary").first().click();
  await pause.focus();
  await page.evaluate(() => {
    window.scrollTo(0, 80);
    window.pageMark = document.querySelector("#chart-default-summary");
  });
  const scrolled = await page.evaluate(() => window.scrollY);

  // First round: nothing changed, and the page has no validator yet, so bodies arrive and nothing
  // is touched. The mark is the very element from the load, so a swap would show.
  let from = polls.length;
  await sleep(POLL_MS + SLACK_MS);
  check(askedSince(from) >= 3, `the first round asked (${askedSince(from)} asks)`);
  check(
    await page.evaluate(() => window.pageMark.isConnected),
    "an answer that changes nothing swaps nothing",
  );
  check(!/Counts updated/.test(await status()), "and says nothing");

  // Second round: conditional, and a body costs nothing.
  from = polls.length;
  await sleep(POLL_MS);
  const second = polls.slice(from);
  check(
    second.length >= 3 && second.every((p) => p.sent && p.status === 304 && p.bytes === 0),
    `an unchanged fragment is a 304 with no body: ${JSON.stringify(second)}`,
  );
  check(!(await page.locator("#status").textContent()).includes("Counts"), "a 304 says nothing");

  // A close, seen with no reload, focus and scroll where they were, the table still open.
  await act({ _ledger: "default", _action: "close", item: "1", reason: "done" });
  await page.waitForFunction(
    () => /2 open, 1 closed/.test(document.querySelector("#chart-default-summary").textContent),
    null,
    { timeout: POLL_MS + SLACK_MS },
  );
  check(true, "a close updates the chart with no reload");
  check(
    !(await page.evaluate(() => window.pageMark.isConnected)),
    "by replacing the fragment",
  );
  check(
    await page.evaluate(() => document.activeElement?.id === "dashboard-pause"),
    "focus stays on the control it was on",
  );
  check(await page.evaluate(() => window.scrollY) === scrolled, "the page does not scroll");
  check(
    await page.locator("details.chart-data").first().evaluate((d) => d.open),
    "an open data table stays open",
  );
  check(
    await page.evaluate(() => document.getAnimations().length === 0),
    "no animation runs, with reduced motion asked for",
  );
  await page.waitForFunction(
    () => /Counts updated/.test(document.querySelector("#status").textContent),
    null,
    { timeout: 3_000 },
  );
  check(
    await page.locator("#status .ui-sr-only", { hasText: "Counts updated." }).count() === 1,
    "one polite sentence, in the page's one status region",
  );
  check(
    await page.locator("#main [aria-busy=true]").count() === 0,
    "nothing is left marked busy",
  );

  // A panel holding focus waits for it, and catches up after.
  const link = page.locator("#chart-default-title a");
  await link.focus();
  await act({ _ledger: "default", _action: "close", item: "2", reason: "done" });
  await sleep(POLL_MS + SLACK_MS);
  check(
    await link.evaluate((a) => a === document.activeElement),
    "focus inside the panel is not taken",
  );
  check(/2 open, 1 closed/.test(await chart()), "and that panel waits");
  await pause.focus();
  await page.waitForFunction(
    () => /1 open, 2 closed/.test(document.querySelector("#chart-default-summary").textContent),
    null,
    { timeout: POLL_MS + SLACK_MS },
  );
  check(true, "the panel catches up once focus has left");
  check(
    await page.evaluate(() => document.activeElement?.id === "dashboard-pause"),
    "with focus still where the person put it",
  );

  // Pause: asks nothing, says so in words, survives a reload; resume asks at once.
  await pause.click();
  const resume = page.getByRole("button", { name: "Resume updates" });
  check(
    await resume.evaluate((b) => b === document.activeElement),
    "the control keeps focus as its name changes",
  );
  check(
    /paused/.test(await page.locator("#dashboard-live-state").textContent()),
    "the pause is said on screen",
  );
  check(/Updates paused/.test(await status()), "and in the status region");
  await act({ _ledger: "default", _action: "close", item: "3", reason: "done" });
  from = polls.length;
  await sleep(POLL_MS + SLACK_MS);
  check(askedSince(from) === 0, "paused, nothing is asked");
  check(/1 open, 2 closed/.test(await chart()), "and the numbers stay");
  await page.reload();
  await resume.waitFor();
  check(true, "the pause survives a reload, remembered by this browser");
  from = polls.length;
  await sleep(POLL_MS + SLACK_MS);
  check(askedSince(from) === 0, "and still asks nothing");
  await resume.click();
  await pause.waitFor();
  await sleep(SLACK_MS);
  check(askedSince(from) >= 3, "resuming asks at once");

  // A hidden tab asks nothing, and asks once it shows again.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  from = polls.length;
  await sleep(POLL_MS + SLACK_MS);
  check(askedSince(from) === 0, "a hidden tab asks nothing");
  await page.evaluate(() => {
    delete document.hidden;
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await sleep(SLACK_MS);
  check(askedSince(from) >= 3, "showing it again asks at once");

  // ★ A table with agents: its links name the minute, so its bytes change with no number moving.
  // That answer is a 200 with a body, and nothing on the page may notice.
  await act({ _ledger: "default", _action: "append", content: "fourth sample" });
  await act({ _ledger: "default", _action: "claim", item: "4", content: "sample-agent" });
  const table = page.locator("section.agent-panel", { hasText: "sample-agent" });
  await table.waitFor({ timeout: POLL_MS + SLACK_MS });
  check(true, "a claim brings the agent's row with no reload");
  await sleep(SLACK_MS);
  await page.evaluate(() => {
    window.tableMark = document.querySelector("section.agent-panel");
    window.saidMark = document.querySelector("#status").lastElementChild;
  });
  from = polls.length;
  await sleep(2 * POLL_MS + 35_000);
  const bodies = polls.slice(from).filter((p) =>
    p.path === "/dashboard/agents/default" && p.status === 200 && p.bytes > 0
  );
  check(bodies.length >= 1, `the minute's new links came as a body (${bodies.length})`);
  check(
    await page.evaluate(() => window.tableMark.isConnected),
    "and an answer that differs only in its links swaps nothing",
  );
  check(
    await page.evaluate(() => window.saidMark === document.querySelector("#status").lastElementChild),
    "and says nothing",
  );

  // Axe and the width, with the control on screen, in each theme.
  for (const theme of ["auto", "light", "dark", "high-contrast"]) {
    for (const width of [1280, 320]) {
      await page.setViewportSize({ width, height: 800 });
      await page.goto(`${base}/dashboard?theme=${theme}`);
      await pause.waitFor();
      const axe = await new AxeBuilder({ page })
        .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
        .analyze();
      check(
        axe.violations.length === 0,
        `axe, ${theme} at ${width}: ${axe.violations.map((v) => v.id).join(", ") || "no violation"}`,
      );
      const wide = await page.evaluate(() => document.documentElement.scrollWidth);
      check(wide <= width, `${theme} at ${width}: scrollWidth ${wide}`);
    }
  }

  // With no script the page is whole, as of its load, with no control to press.
  const off = await browser.newContext({ javaScriptEnabled: false });
  const still = await off.newPage();
  await still.goto(`${base}/dashboard`);
  check(
    /as of when this page loaded/.test(await still.locator("#dashboard-live-state").textContent()),
    "with no script the page says it is as of its load",
  );
  check(!(await still.locator("#dashboard-pause").isVisible()), "and shows no control");
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
