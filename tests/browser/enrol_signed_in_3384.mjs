// Ledger #3384: a passkey created while signed in ends the old session, and a label another
// passkey already has is refused before the authenticator is asked.
//
// deno run -A tests/browser/enrol_signed_in_3384.mjs
//
// The page's own script (web/gonk.js) over a fixture header with gonk's ids, the /auth endpoints
// answered by the route below, and navigator.credentials.create() stubbed: the system passkey
// sheet is the operating system's, not the page's.
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

async function open({ signedIn, taken = [] }) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const calls = [];
  const sessions = new Set(signedIn ? ["old-token"] : []);
  await page.addInitScript(() => {
    window.PublicKeyCredential = function () {};
    window.creates = 0;
    navigator.credentials.create = async () => {
      window.creates++;
      return {
        id: "bmV3",
        response: {
          getPublicKey: () => new Uint8Array([1, 2, 3]).buffer,
          clientDataJSON: new Uint8Array([4]).buffer,
        },
      };
    };
  });
  if (signedIn) {
    await context.addCookies([{ name: "gonk_session", value: "old-token", url: "http://localhost:9/" }]);
  }
  await page.route("http://localhost:9/**", (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const body = req.postData() ?? "";
    calls.push({ path, body });
    const json = (status, value) =>
      route.fulfill({ status, contentType: "application/json", body: JSON.stringify(value) });
    switch (path) {
      case "/gonk.js":
        return route.fulfill({ contentType: "text/javascript", body: gonk });
      case "/auth/session":
        return sessions.has(body)
          ? json(200, { label: "old laptop", grant: "reader" })
          : route.fulfill({ status: 401, body: "no session" });
      case "/auth/logout":
        sessions.delete(body);
        return json(200, {});
      case "/auth/register-options": {
        const label = JSON.parse(body || "{}").label;
        if (taken.includes(label)) {
          return route.fulfill({
            status: 400,
            body: `another passkey is already labeled \`${label}\` — choose a different label; the invite is still good`,
          });
        }
        return json(200, { challenge: "Y2hhbGxlbmdl", rpId: "localhost" });
      }
      case "/auth/register":
        return json(200, { label: JSON.parse(body).label, grant: "writer" });
      default:
        return route.fulfill({ contentType: "text/html", body: PAGE });
    }
  });
  await page.goto("http://localhost:9/l/default#invite=abc");
  await page.waitForFunction(() => !document.getElementById("enrol").hidden);
  if (signedIn) await page.waitForFunction(() => !document.getElementById("auth-logout").hidden);
  return { context, page, calls, sessions, errors };
}

const state = (page) =>
  page.evaluate(() => ({
    status: document.getElementById("status").textContent,
    focus: document.activeElement && document.activeElement.id,
    login: !document.getElementById("auth-login").hidden,
    logout: !document.getElementById("auth-logout").hidden,
    who: document.getElementById("auth-who").hidden ? "" : document.getElementById("auth-who").textContent,
    cookie: document.cookie,
    creates: window.creates,
  }));

await check("a passkey created while signed in ends the old session and focuses Sign in", async () => {
  const { context, page, calls, sessions, errors } = await open({ signedIn: true });
  await page.fill("#enrol-label", "phone");
  await page.click("#enrol-form button");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Passkey created"));
  const s = await state(page);
  assert.equal(s.cookie, "", "the session cookie is gone");
  assert.equal(sessions.size, 0, "the server was told: the old session is ended there too");
  assert.ok(calls.some((c) => c.path === "/auth/logout" && c.body === "old-token"));
  assert.equal(s.logout, false, "no Sign out left for a session that is over");
  assert.equal(s.who, "", "no 'Signed in as' the old passkey");
  assert.equal(s.login, true);
  assert.equal(s.focus, "auth-login", "one click, the same as signed out");
  assert.match(s.status, /old laptop \(grant reader\) is signed out\. Now click Sign in with passkey/);
  assert.deepEqual(errors, []);
  await context.close();
});

await check("signed out, a created passkey still focuses Sign in", async () => {
  const { context, page, calls, errors } = await open({ signedIn: false });
  await page.fill("#enrol-label", "phone");
  await page.click("#enrol-form button");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Passkey created"));
  const s = await state(page);
  assert.equal(s.focus, "auth-login");
  assert.ok(!calls.some((c) => c.path === "/auth/logout"));
  assert.match(s.status, /^Passkey created for phone \(grant writer\)\. Now click Sign in/);
  assert.deepEqual(errors, []);
  await context.close();
});

await check("a used label is refused before the authenticator is asked, the form keeps focus", async () => {
  const { context, page, calls, sessions, errors } = await open({ signedIn: true, taken: ["laptop"] });
  await page.fill("#enrol-label", "laptop");
  await page.click("#enrol-form button");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Could not create"));
  const s = await state(page);
  assert.equal(s.creates, 0, "no credential was minted");
  assert.ok(!calls.some((c) => c.path === "/auth/register"));
  assert.match(s.status, /already labeled `laptop` — choose a different label/);
  assert.equal(s.focus, "enrol-label");
  assert.equal(sessions.size, 1, "a refusal ends nothing");
  // Another label, the same invite.
  await page.fill("#enrol-label", "phone");
  await page.click("#enrol-form button");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Passkey created"));
  assert.equal((await state(page)).creates, 1);
  assert.deepEqual(errors, []);
  await context.close();
});

await browser.close();
if (failures) {
  console.log(`${failures} failed`);
  Deno.exit(1);
}
console.log("all passed");
