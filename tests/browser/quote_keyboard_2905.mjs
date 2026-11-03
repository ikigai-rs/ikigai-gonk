// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/quote_keyboard_2905.mjs [target/debug/ikigai-gonk]
//
// A keyboard-only walk of gonk's served file view (ledger #2905): a scratch gonk, a virtual passkey
// to sign in, then Tab, typing and Enter alone. Since ikigai-browse 0.20.6 a gutter link is out of
// the tab order, so the line field in the annotate form is how a keyboard fills the quote.
import { chromium } from "npm:playwright@1.64.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-quote-keyboard-" });
const lines = Array.from({ length: 80 }, (_, i) => `// line ${i + 1}`);
lines[2] = "    let quoted = true;";
lines[3] = "";
const text = lines.join("\n") + "\n";
await Deno.mkdir(`${scratch}/repo/src`, { recursive: true });
await Deno.writeTextFile(`${scratch}/repo/src/lib.rs`, text);
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(
  `${scratch}/config/config.toml`,
  `gonk.port = ${port}\n` +
    `gonk.browse.root = "demo=${scratch}/repo"\n` +
    `gonk.mount = "prefer urn:llm:=${scratch}/no-model.sock"\n`,
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
  const invited = await new Deno.Command(binary, {
    args: [
      "passkey",
      "invite",
      "keyboard",
      "--ledger",
      "default=read",
      "--browse",
      "derive",
      ...homes,
    ],
  }).output();
  const said = new TextDecoder().decode(invited.stdout);
  assert.ok(
    invited.success,
    said + new TextDecoder().decode(invited.stderr),
  );
  const invite = said.match(/http:\/\/\S*#invite=\S+/)[0];

  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const cdp = await page.context().newCDPSession(page);
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

  // The setup may use anything; the walk below it uses only the keyboard.
  await page.goto(invite);
  await page.fill("#enrol-label", "keyboard");
  await page.locator("#enrol-form button[type=submit]").click();
  await page.locator("#status .flash.ok").waitFor();
  await page.locator("#auth-login").click();
  await page.locator("#auth-who").waitFor();

  const file = `${base}/browse/urn:repo:demo:file:src/lib.rs`;
  const exact = page.locator('form.browse-annotate input[name="exact"]');
  const field = (name) =>
    page.locator(`form.browse-annotate input[name="${name}"]`).inputValue();
  const status = () => page.locator("#status").innerText();
  const open = async (url) => {
    await page.goto("about:blank");
    await page.goto(url);
    await exact.waitFor();
  };
  // What holds focus, as the same few words each time.
  const focused = () =>
    page.evaluate(() => {
      const el = document.activeElement;
      if (el.matches("a.browse-ln")) return "gutter";
      if (el.matches("pre.browse-code")) return "code";
      if (el.closest("form.browse-annotate")) {
        return el.getAttribute("name") || el.className || el.tagName;
      }
      return "other";
    });
  const tabTo = async (name) => {
    const seen = [];
    for (let i = 0; i < 80; i++) {
      await page.keyboard.press("Tab");
      const now = await focused();
      seen.push(now);
      if (now === name) return seen;
    }
    assert.fail(`Tab never reached ${name}: ${seen.join(" ")}`);
  };
  const settled = (name, value) =>
    page.waitForFunction(
      ([name, value]) =>
        document.querySelector(`form.browse-annotate input[name="${name}"]`)
          .value === value,
      [name, value],
    );

  // Tab cannot reach a line, and the whole file is one stop.
  await open(file);
  const seen = await tabTo("exact");
  assert.equal(seen.filter((stop) => stop === "gutter").length, 0, seen.join(" "));
  assert.equal(seen.filter((stop) => stop === "code").length, 1, seen.join(" "));
  assert.equal(
    await page.locator("a.browse-ln").first().getAttribute("tabindex"),
    "-1",
  );
  assert.equal(await field("exact"), "");
  console.log(`Tab reaches no line, and the quote stays empty: ${seen.join(" ")}`);

  // The keyboard path: type a line number, press Enter.
  const LINE = "gonk-quote-line";
  const GO = "gonk-quote-go";
  await open(file);
  await tabTo(LINE);
  await page.keyboard.type("3");
  await page.keyboard.press("Enter");
  await settled("exact", "let quoted = true;");
  assert.equal(await field("prefix"), "// line 2\n    ".slice(-32));
  assert.equal(await field("suffix"), "\n");
  assert.equal(new URL(page.url()).hash, "#L3");
  assert.ok(await page.evaluate(() => document.getElementById("L3").matches(":target")));
  assert.equal(await focused(), LINE, "focus stays where the person pressed Enter");
  assert.match(await status(), /The annotate quote is now line 3\./);
  assert.equal(
    await page.locator(".browse-annotated-note").count(),
    0,
    "Enter in the line field does not submit the annotation",
  );

  // The same from the button, by Space, over a quote the hook filled.
  await page.keyboard.press("Control+A");
  await page.keyboard.type("5");
  await page.keyboard.press("Tab");
  assert.equal(await focused(), GO);
  await page.keyboard.press("Space");
  await settled("exact", "// line 5");
  assert.equal(await focused(), GO);
  assert.match(await status(), /The annotate quote is now line 5\./);

  // A line with nothing to quote, and a line the file does not have, are said and change nothing.
  await page.keyboard.press("Shift+Tab");
  for (const [wanted, sentence] of [
    ["4", /Line 4 is blank/],
    ["999", /This file has 80 lines/],
    ["abc", /line number/],
  ]) {
    await page.keyboard.press("Control+A");
    await page.keyboard.type(wanted);
    await page.keyboard.press("Enter");
    assert.match(await status(), sentence);
    assert.equal(await field("exact"), "// line 5");
    assert.equal(new URL(page.url()).hash, "#L5");
    assert.equal(await focused(), LINE);
  }

  // A deep link's line is already in the field, so the button quotes the :target line.
  await open(`${file}#L7`);
  await settled("exact", "// line 7");
  await page.waitForFunction(
    (line) => document.querySelector(`input.${line}`)?.value === "7",
    LINE,
  );
  await tabTo("exact");
  await page.keyboard.press("Control+A");
  await page.keyboard.press("Delete");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await focused(), GO);
  await page.keyboard.press("Enter");
  await settled("exact", "// line 7");
  assert.match(await status(), /The annotate quote is now line 7\./);

  // A quote the person typed is never overwritten, and the field says why nothing moved.
  await page.keyboard.press("Tab");
  await page.keyboard.press("Control+A");
  await page.keyboard.type("my own words");
  await page.keyboard.press("Shift+Tab");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await focused(), LINE);
  await page.keyboard.press("Control+A");
  await page.keyboard.type("9");
  await page.keyboard.press("Enter");
  assert.equal(await field("exact"), "my own words");
  assert.equal(new URL(page.url()).hash, "#L7");
  assert.match(await status(), /you typed/);

  // The whole path: quote a line and annotate it, by keyboard. The view that comes back has the
  // new card focused, and one line field, not two.
  await open(file);
  await tabTo(LINE);
  await page.keyboard.type("3");
  await page.keyboard.press("Enter");
  await settled("exact", "let quoted = true;");
  await tabTo("body");
  await page.keyboard.type("Annotated without a mouse.");
  await tabTo("BUTTON");
  await page.keyboard.press("Enter");
  await page.locator(".browse-annotation-new").waitFor();
  await page.waitForFunction(() =>
    document.activeElement?.matches(".browse-annotation-new")
  );
  assert.match(
    await page.locator(".browse-annotation-new").innerText(),
    /Annotated without a mouse\./,
  );
  await page.locator(`input.${LINE}`).waitFor();
  assert.equal(await page.locator(`input.${LINE}`).count(), 1);
  assert.equal(await page.locator("#L3.browse-line-annotated").count(), 1);

  // A pointer's path is unchanged, and the line field follows it.
  await open(file);
  await page.locator('a.browse-ln[href="#L6"]').click();
  await settled("exact", "// line 6");
  assert.equal(await page.locator(`input.${LINE}`).inputValue(), "6");
  assert.match(await status(), /The annotate quote is now line 6\./);

  assert.deepEqual(errors, []);
  console.log(`The keyboard fills the annotate quote in ${browser.version()}`);
} finally {
  await browser.close();
  server.kill();
  await server.status;
  await Deno.remove(scratch, { recursive: true }).catch(() => {});
}
