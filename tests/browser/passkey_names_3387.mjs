// Ledger #3387: the system passkey picker shows a passkey's WebAuthn `user.name` and
// `user.displayName`, and both were the bare label, so two gonks' passkeys (every gonk is the
// `localhost` relying party) and two grants read alike. A passkey is now named by its label and
// host ("laptop @ localhost:9") and displayed with its grant ("laptop · writer"), and each
// sign-in signals the current label and grant where the browser has
// `PublicKeyCredential.signalCurrentUserDetails`.
//
// deno run -A tests/browser/passkey_names_3387.mjs
//
// The page's own script (web/gonk.js) over a fixture header with gonk's ids, the /auth endpoints
// answered by the route below, and navigator.credentials stubbed: the system passkey sheet is
// the operating system's, not the page's.
import { chromium } from "npm:playwright@1.64.0";
import assert from "node:assert/strict";

const gonk = await Deno.readTextFile(new URL("../../web/gonk.js", import.meta.url));
const PAGE = `<!doctype html><html lang="en"><head><title>gonk</title>
<script src="/gonk.js" defer></script></head><body>
<header><div id="auth" class="auth">
  <span id="auth-who" class="who" hidden="hidden"></span>
  <button id="auth-login" type="button" hidden="hidden">Sign in with passkey</button>
  <button id="auth-logout" type="button" hidden="hidden">Sign out</button>
  <a id="auth-localhost" hidden="hidden">Passkeys need localhost</a>
</div></header>
<section id="enrol" hidden="hidden"><form id="enrol-form">
  <label for="enrol-label">Label</label><input id="enrol-label" type="text"/>
  <button type="submit">Create passkey</button>
</form></section>
<div id="status" role="status" aria-live="polite"></div>
<main id="main"><h1>Ledger</h1></main></body></html>`;

const browser = await chromium.launch();
let failures = 0;
const check = async (name, fn) => {
  try {
    await fn();
    console.log("ok  ", name);
  } catch (e) {
    failures++;
    console.log("FAIL", name, "\n", e.message);
  }
};

async function open({ signal, hash = "", grant = "owner-viewer", label = "laptop" }) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const calls = [];
  await page.addInitScript((signal) => {
    window.PublicKeyCredential = function () {};
    if (signal) {
      window.signals = [];
      window.PublicKeyCredential.signalCurrentUserDetails = async (details) => {
        window.signals.push(details);
      };
    }
    window.created = [];
    navigator.credentials.create = async (options) => {
      const u = options.publicKey.user;
      window.created.push({ name: u.name, displayName: u.displayName, rp: options.publicKey.rp });
      return {
        id: "bmV3",
        response: {
          getPublicKey: () => new Uint8Array([1, 2, 3]).buffer,
          clientDataJSON: new Uint8Array([4]).buffer,
        },
      };
    };
    navigator.credentials.get = async () => ({
      id: "bmV3",
      response: {
        authenticatorData: new Uint8Array([5]).buffer,
        clientDataJSON: new Uint8Array([6]).buffer,
        signature: new Uint8Array([7]).buffer,
        userHandle: new Uint8Array([0xfb, 0xff, 0x01]).buffer,
      },
    });
  }, signal);
  await page.route("http://localhost:9/**", (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const body = req.postData() ?? "";
    calls.push({ path, body });
    const json = (status, value) =>
      route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) });
    switch (path) {
      case "/gonk.js":
        return route.fulfill({ contentType: "text/javascript; charset=utf-8", body: gonk }); // as gonk serves it
      case "/auth/session":
        return body === "tok"
          ? json(200, { label, grant, scopes: [], seconds: 60 })
          : route.fulfill({ status: 401, body: "no session" });
      case "/auth/register-options":
        return json(200, { challenge: "Y2hhbGxlbmdl", rpId: "localhost", grant });
      case "/auth/register":
        return json(200, { label: JSON.parse(body).label, grant });
      case "/auth/login-options":
        return json(200, { challenge: "Y2hhbGxlbmdl", rpId: "localhost" });
      case "/auth/login":
        return json(200, { session: "tok", seconds: 60, label, grant });
      default:
        return route.fulfill({ contentType: "text/html", body: PAGE });
    }
  });
  await page.goto("http://localhost:9/l/default" + hash);
  await page.waitForFunction(() => !document.getElementById("auth-login").hidden);
  // The first load done, so a `load` waited for after a click is the sign-in's reload.
  await page.waitForLoadState("load");
  return { context, page, calls, errors };
}

async function enrol(page, label) {
  await page.waitForFunction(() => !document.getElementById("enrol").hidden);
  await page.fill("#enrol-label", label);
  await page.click("#enrol-form button");
  await page.waitForFunction(() => /Passkey created|Could not/.test(document.getElementById("status").textContent));
  return page.evaluate(() => window.created);
}

await check("creation names the passkey by label and host, and displays its grant", async () => {
  const { context, page, calls, errors } = await open({ hash: "#invite=abc" });
  const created = await enrol(page, "laptop");
  assert.deepEqual(created.map(({ name, displayName }) => ({ name, displayName })), [
    { name: "laptop @ localhost:9", displayName: "laptop · owner-viewer" },
  ]);
  assert.equal(created[0].rp.id, "localhost");
  const asked = calls.find((c) => c.path === "/auth/register-options");
  assert.deepEqual(JSON.parse(asked.body), { label: "laptop", invite: "abc" }, "the grant is asked for by invite");
  const everything = JSON.stringify(created);
  assert.ok(!everything.includes("abc"), "the invite code is in neither name");
  assert.deepEqual(errors, []);
  await context.close();
});

await check("both names stay within 64 bytes, the host and grant kept whole", async () => {
  const label = "a very long label for the passkey on my work laptop, ünïcödé ✓✓✓✓✓✓";
  const { context, page, errors } = await open({ hash: "#invite=abc", grant: "release-manager" });
  const [{ name, displayName }] = await enrol(page, label);
  const bytes = (s) => new TextEncoder().encode(s).length;
  assert.ok(bytes(name) <= 64, `${bytes(name)}: ${name}`);
  assert.ok(bytes(displayName) <= 64, `${bytes(displayName)}: ${displayName}`);
  assert.ok(name.endsWith("… @ localhost:9"), name);
  assert.ok(displayName.endsWith("… · release-manager"), displayName);
  assert.ok(name.startsWith("a very long label"), name);
  assert.ok(!/�/.test(name + displayName), "no character is cut in half");
  assert.deepEqual(errors, []);
  await context.close();
});

await check("a sign-in signals the user handle, rpId, and the current label and grant", async () => {
  const { context, page, errors } = await open({ signal: true, label: "laptop", grant: "owner" });
  // Hold the reload so the stub's record survives to be read.
  await page.evaluate(() => {
    window.signalled = new Promise((resolve) => {
      const real = PublicKeyCredential.signalCurrentUserDetails;
      PublicKeyCredential.signalCurrentUserDetails = async (d) => {
        await real(d);
        resolve(d);
        await new Promise(() => {}); // never settles: the page stays for the read
      };
    });
  });
  await page.click("#auth-login");
  const details = await page.evaluate(() => window.signalled);
  assert.deepEqual(details, {
    rpId: "localhost",
    userId: "-_8B",
    name: "laptop @ localhost:9",
    displayName: "laptop · owner",
  });
  assert.deepEqual(errors, []);
  await context.close();
});

await check("a signal that never settles does not hold the reload", async () => {
  const { context, page, errors } = await open({ signal: true });
  await page.evaluate(() => {
    PublicKeyCredential.signalCurrentUserDetails = () => new Promise(() => {});
  });
  const reloaded = page.waitForEvent("load", { timeout: 5000 });
  await page.click("#auth-login");
  await reloaded;
  assert.deepEqual(errors, []);
  await context.close();
});

await check("without the signal a sign-in completes as before, silently", async () => {
  const { context, page, calls, errors } = await open({ signal: false });
  const reloaded = page.waitForEvent("load");
  await page.click("#auth-login");
  await reloaded;
  await page.waitForFunction(() => !document.getElementById("auth-who").hidden);
  const cookies = await context.cookies();
  assert.ok(cookies.some((c) => c.name === "gonk_session" && c.value === "tok"), "signed in");
  assert.ok(calls.some((c) => c.path === "/auth/session" && c.body === "tok"), "the reloaded page asked as the new session");
  assert.equal(await page.evaluate(() => document.getElementById("status").textContent), "");
  assert.deepEqual(errors, []);
  await context.close();
});

await check("a signal that rejects does not stop the sign-in", async () => {
  const { context, page, calls, errors } = await open({ signal: true });
  await page.evaluate(() => {
    PublicKeyCredential.signalCurrentUserDetails = async () => {
      throw new DOMException("no", "NotSupportedError");
    };
  });
  const reloaded = page.waitForEvent("load");
  await page.click("#auth-login");
  await reloaded;
  await page.waitForFunction(() => !document.getElementById("auth-who").hidden);
  const cookies = await context.cookies();
  assert.ok(cookies.some((c) => c.name === "gonk_session" && c.value === "tok"), "signed in");
  assert.ok(calls.some((c) => c.path === "/auth/session" && c.body === "tok"), "the reloaded page asked as the new session");
  assert.deepEqual(errors, []);
  await context.close();
});

await browser.close();
if (failures) {
  console.log(`${failures} failed`);
  Deno.exit(1);
}
console.log("all passed");
