// Ledger #3388: signed in, an invite link offers to add its grant to the passkey signed in with
// (the default), against a fresh assertion by that passkey, or to create a new passkey.
//
// deno run -A tests/browser/add_grant_3388.mjs
//
// The page's own script (web/gonk.js) over a fixture with gonk's ids (the invite panel as
// web/gonk.xsl draws it), the /auth endpoints answered by the route below, and
// navigator.credentials stubbed: the system passkey sheet is the operating system's.
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
<section id="enrol" hidden="hidden">
  <p id="enrol-signed-in" hidden="hidden">You are signed in.</p>
  <form id="enrol-form">
    <fieldset id="enrol-choice" hidden="hidden">
      <legend>What should this invite do?</legend>
      <label><input type="radio" name="enrol-mode" id="enrol-add" value="add" checked="checked"/> Add its grant to the passkey you are signed in with</label>
      <label><input type="radio" name="enrol-mode" id="enrol-create" value="create"/> Create a new passkey</label>
    </fieldset>
    <span id="enrol-new"><label for="enrol-label">Label</label><input id="enrol-label" type="text"/></span>
    <button type="submit" id="enrol-submit">Create passkey</button>
  </form>
</section>
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

async function open({ signedIn, refuse = false }) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const calls = [];
  const sessions = new Map(signedIn ? [["old-token", { label: "laptop", grant: "viewer" }]] : []);
  await page.addInitScript(() => {
    window.PublicKeyCredential = function () {};
    window.gets = [];
    window.creates = 0;
    const buf = (bytes) => new Uint8Array(bytes).buffer;
    navigator.credentials.get = async (options) => {
      window.gets.push(
        (options.publicKey.allowCredentials || []).map((c) => Array.from(new Uint8Array(c.id)))
      );
      return {
        id: "bGFwdG9w",
        response: { authenticatorData: buf([1]), clientDataJSON: buf([2]), signature: buf([3]) },
      };
    };
    navigator.credentials.create = async () => {
      window.creates++;
      return {
        id: "bmV3",
        response: { getPublicKey: () => buf([1, 2, 3]), clientDataJSON: buf([4]) },
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
        return sessions.has(body) ? json(200, sessions.get(body)) : route.fulfill({ status: 404, body: "no session" });
      case "/auth/logout":
        sessions.delete(body);
        return json(200, {});
      case "/auth/add-grant-options": {
        const asked = JSON.parse(body);
        if (!sessions.has(asked.session)) return route.fulfill({ status: 400, body: "you are not signed in" });
        return json(200, { challenge: "Y2hhbGxlbmdl", rpId: "localhost", grant: "writer", credential: "bGFwdG9w" });
      }
      case "/auth/add-grant": {
        if (refuse) return route.fulfill({ status: 403, body: "the passkey assertion was refused (user not verified)" });
        const asked = JSON.parse(body);
        assert.ok(asked.signature && asked.authenticatorData && asked.clientDataJSON, "a fresh assertion is sent");
        sessions.delete(asked.session);
        sessions.set("new-token", { label: "laptop", grant: "viewer, writer" });
        return json(200, {
          session: "new-token",
          seconds: 3600,
          label: "laptop",
          grant: "viewer, writer",
          grants: ["viewer", "writer"],
          added: "writer",
        });
      }
      case "/auth/register-options":
        return json(200, { challenge: "Y2hhbGxlbmdl", rpId: "localhost", grant: "writer" });
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
    who: document.getElementById("auth-who").hidden ? "" : document.getElementById("auth-who").textContent,
    cookie: document.cookie,
    choice: !document.getElementById("enrol-choice").hidden,
    note: !document.getElementById("enrol-signed-in").hidden,
    label: !document.getElementById("enrol-new").hidden,
    button: document.getElementById("enrol-submit").textContent,
    add: document.getElementById("enrol-add").checked,
    gets: window.gets,
    creates: window.creates,
    hash: location.hash,
  }));

await check("signed in, adding is the default and asks the signed-in passkey for a fresh assertion", async () => {
  const { context, page, calls, sessions, errors } = await open({ signedIn: true });
  let s = await state(page);
  assert.equal(s.choice, true, "the two choices are shown");
  assert.equal(s.note, true);
  assert.equal(s.add, true, "add is the default");
  assert.equal(s.label, false, "no Label field for adding");
  assert.equal(s.button, "Add grant to this passkey");
  await page.click("#enrol-submit");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("added"));
  s = await state(page);
  assert.equal(s.cookie, "gonk_session=new-token", "the new session replaced the old");
  assert.ok(!sessions.has("old-token"));
  assert.equal(s.hash, "", "the spent invite left the address");
  assert.equal(s.who, "Signed in as laptop (grant viewer, writer)", "the reloaded page reads the wider grant");
  assert.match(s.status, /^Grant writer added to the passkey laptop, which now holds viewer, writer\./);
  assert.deepEqual(s.gets, [], "the stub is per page: the reload reset it");
  const sent = calls.find((c) => c.path === "/auth/add-grant");
  assert.ok(sent, "add-grant was posted");
  const body = JSON.parse(sent.body);
  assert.equal(body.session, "old-token");
  assert.equal(body.invite, "abc");
  assert.equal(body.id, "bGFwdG9w");
  assert.ok(!calls.some((c) => c.path === "/auth/register" || c.path === "/auth/logout"));
  assert.deepEqual(errors, []);
  await context.close();
});

await check("the assertion is asked of the signed-in passkey only", async () => {
  const { context, page, errors } = await open({ signedIn: true, refuse: true });
  await page.click("#enrol-submit");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Could not add"));
  const s = await state(page);
  // "bGFwdG9w" is base64url of "laptop".
  assert.deepEqual(s.gets, [[[..."laptop"].map((c) => c.charCodeAt(0))]]);
  assert.match(s.status, /^Could not add the grant: the passkey assertion was refused/);
  assert.equal(s.focus, "enrol-submit", "focus stays on the form's button");
  assert.equal(s.cookie, "gonk_session=old-token", "a refusal ends nothing");
  assert.equal(s.hash, "#invite=abc");
  assert.deepEqual(errors, []);
  await context.close();
});

await check("choosing Create a new passkey shows the Label field and enrolls as before", async () => {
  const { context, page, calls, errors } = await open({ signedIn: true });
  await page.check("#enrol-create");
  let s = await state(page);
  assert.equal(s.label, true);
  assert.equal(s.button, "Create passkey");
  await page.fill("#enrol-label", "phone");
  await page.click("#enrol-submit");
  await page.waitForFunction(() => document.getElementById("status").textContent.includes("Passkey created"));
  s = await state(page);
  assert.equal(s.creates, 1);
  assert.ok(!calls.some((c) => c.path.startsWith("/auth/add-grant")));
  assert.deepEqual(errors, []);
  await context.close();
});

await check("signed out, there is no choice: the Label field and Create passkey", async () => {
  const { context, page, errors } = await open({ signedIn: false });
  const s = await state(page);
  assert.equal(s.choice, false);
  assert.equal(s.note, false);
  assert.equal(s.label, true);
  assert.equal(s.button, "Create passkey");
  assert.deepEqual(errors, []);
  await context.close();
});

await browser.close();
if (failures) {
  console.log(`${failures} failed`);
  Deno.exit(1);
}
console.log("all passed");
