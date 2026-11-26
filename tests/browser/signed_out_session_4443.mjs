// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/signed_out_session_4443.mjs [target/debug/ikigai-gonk]
//
// Ledger #4443, over a scratch gonk: a page loaded by nobody signed in asks the server whether it
// is signed in without a request that fails, so its console stays clean; it offers the sign-in
// button. Signed in over Chromium's virtual authenticator, the same question still finds the
// session: the page says who, offers sign-out, and a reload keeps both. Exits non-zero on any
// failure.
import { chromium } from "npm:playwright@1.64.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-signed-out-session-" });
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(`${scratch}/config/config.toml`, `gonk.port = ${port}\n`);
const homes = ["--config-home", `${scratch}/config`, "--data-home", `${scratch}/data`];
const server = new Deno.Command(binary, {
  args: [...homes, "--no-quic", "--no-backup"],
  stdout: "null",
  stderr: "null",
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
      await new Promise((done) => setTimeout(done, 100));
    }
  }
  console.log(`Chromium ${browser.version()}`);

  // Every console error, page error and error status a page's own requests met.
  const watch = (page) => {
    const seen = { errors: [], failed: [], asked: [] };
    page.on("pageerror", (error) => seen.errors.push(String(error)));
    page.on("console", (message) => {
      if (message.type() === "error") seen.errors.push(message.text());
    });
    page.on("requestfailed", (request) => seen.failed.push(`${request.method()} ${request.url()}`));
    page.on("response", (response) => {
      const url = new URL(response.url());
      if (url.pathname === "/auth/session") seen.asked.push(`${response.request().method()} ${response.status()}`);
      if (response.status() >= 400) seen.failed.push(`${response.status()} ${response.url()}`);
    });
    return seen;
  };
  // gonk.js asks /auth/session, and so does ikigai-ui's shell (its write-lapse check): each a GET,
  // each answered 200. Before ledger #4443 gonk.js's was a POST, which a signed-out page got 404.
  const answered = (asked, said) => {
    assert.ok(asked.length >= 1, `${said}: the page asked`);
    assert.deepEqual(asked.filter((a) => a !== "GET 200"), [], `${said}: every ask was a GET answered 200`);
  };
  const state = (page) =>
    page.evaluate(() => ({
      login: !document.getElementById("auth-login").hidden,
      logout: !document.getElementById("auth-logout").hidden,
      who: document.getElementById("auth-who").hidden ? "" : document.getElementById("auth-who").textContent,
    }));

  // Signed out: every page asks, and none of its requests fails.
  const page = await browser.newPage();
  const seen = watch(page);
  for (const path of ["/", "/l/default", "/l/default/doctor", "/dashboard"]) {
    seen.asked.length = 0;
    await page.goto(`${base}${path}`);
    await page.locator("#auth-login").waitFor();
    assert.deepEqual(await state(page), { login: true, logout: false, who: "" }, path);
    answered(seen.asked, path);
    console.log(`ok signed out ${path}: ${seen.asked.join(", ")}, the sign-in button shows`);
  }
  assert.deepEqual(seen.failed, [], "no request a signed-out page made failed");
  assert.deepEqual(seen.errors, [], "no console or page error");
  console.log("ok a signed-out page logs no error");

  // Signed in: the same question finds the session.
  const invited = await new Deno.Command(binary, {
    args: ["passkey", "invite", "reader", "--ledger", "default=write", ...homes],
  }).output();
  const said = new TextDecoder().decode(invited.stdout);
  assert.ok(invited.success, said + new TextDecoder().decode(invited.stderr));
  const invite = said.match(/http:\/\/\S*#invite=\S+/)[0];
  const context = await browser.newContext();
  const signed = await context.newPage();
  const cdp = await context.newCDPSession(signed);
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
  await signed.goto(invite);
  await signed.fill("#enrol-label", "reader");
  await signed.locator("#enrol-form button[type=submit]").click();
  await signed.locator("#status .flash.ok").waitFor();
  await signed.locator("#auth-login").click();
  await signed.locator("#auth-who").waitFor();
  const inside = watch(signed);
  await signed.goto(`${base}/l/default`);
  await signed.locator("#auth-who").waitFor();
  const shown = await state(signed);
  assert.deepEqual({ ...shown, who: "" }, { login: false, logout: true, who: "" });
  assert.match(shown.who, /^Signed in as reader \(grant .+\)$/);
  answered(inside.asked, "signed in");
  assert.equal((await context.cookies()).find((c) => c.name === "gonk_session")?.httpOnly, true);
  assert.equal(await signed.evaluate(() => document.cookie), "", "the script never sees the token");
  assert.deepEqual(inside.failed, []);
  assert.deepEqual(inside.errors, []);
  console.log(`ok signed in: ${(await state(signed)).who}, sign-out shows, nothing failed`);

  // Signing out leaves the next load signed out, and still clean.
  await signed.locator("#auth-logout").click();
  await signed.locator("#auth-login").waitFor();
  inside.asked.length = 0;
  await signed.reload();
  await signed.locator("#auth-login").waitFor();
  assert.deepEqual(await state(signed), { login: true, logout: false, who: "" });
  answered(inside.asked, "signed out again");
  assert.deepEqual(inside.failed, []);
  assert.deepEqual(inside.errors, []);
  console.log("ok signed out again: the reload asks, is answered, and logs no error");
  console.log("all passed");
} finally {
  await browser.close();
  server.kill();
  await server.status;
  await Deno.remove(scratch, { recursive: true }).catch(() => {});
}
