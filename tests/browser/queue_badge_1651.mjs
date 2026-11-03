// Run after `cargo build`, from the repository root:
// deno run -A tests/browser/queue_badge_1651.mjs [target/debug/ikigai-gonk]
//
// Ledger #1651, over a scratch gonk with an armed queue and a virtual passkey: what the Queue badge
// says (the computed name and description, its numbers' contrast through every ancestor's opacity,
// and a stuck queue's visible word), axe and the 320 px page in every theme, focus after a form's
// swap, and the ledger's forms in a browser with JavaScript off. Exits non-zero on any failure.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";

const binary = Deno.args[0] ?? "target/debug/ikigai-gonk";
const port = 20000 + Math.floor(Math.random() * 20000);
const scratch = await Deno.makeTempDir({ prefix: "gonk-queue-badge-" });
await Deno.mkdir(`${scratch}/repo/src`, { recursive: true });
await Deno.writeTextFile(`${scratch}/repo/src/lib.rs`, "// a file\n");
await Deno.mkdir(`${scratch}/config`);
await Deno.writeTextFile(
  `${scratch}/config/config.toml`,
  `gonk.port = ${port}\n` +
    `gonk.browse.root = "demo=${scratch}/repo"\n` +
    `gonk.mount = "prefer urn:llm:=${scratch}/no-model.sock"\n` +
    `gonk.review.space = "reviews"\n` +
    `gonk.review.root = "${scratch}/spaces"\n` +
    `gonk.review.arm = true\n` +
    `gonk.review.grant = "reviewer"\n`,
);
const homes = [
  "--config-home",
  `${scratch}/config`,
  "--data-home",
  `${scratch}/data`,
];

// Arming is a grant plus a word: the reviewer grant the server's own refusal spells out.
await Deno.mkdir(`${scratch}/config/gonk`);
await Deno.writeTextFile(
  `${scratch}/config/gonk/grants.json`,
  JSON.stringify({
    reviewer: [
      "urn:cap:browse:read:*",
      "urn:cap:net:localhost",
      "urn:cap:store:read:graph:urn:iki:browse:graph:default",
      "urn:cap:store:write:graph:urn:iki:browse:graph:default",
      "urn:cap:exec:gh",
    ],
  }),
);

const server = new Deno.Command(binary, {
  args: [...homes, "--no-quic", "--no-backup"],
  stdout: "null",
  stderr: "null",
}).spawn();

// WCAG's relative luminance and ratio, over the colors a browser computed.
const parse = (css) => {
  const [r, g, b, a = 1] = css.match(/[\d.]+/g).map(Number);
  return { r, g, b, a };
};
const over = (top, ground, alpha = top.a) => ({
  r: top.r * alpha + ground.r * (1 - alpha),
  g: top.g * alpha + ground.g * (1 - alpha),
  b: top.b * alpha + ground.b * (1 - alpha),
  a: 1,
});
const luminance = ({ r, g, b }) => {
  const channel = (c) => {
    c /= 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
};
const ratio = (a, b) => {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
};

// The text's color through its element's whole stack of opacity, on the first ground that is one.
const contrast = async (locator) => {
  const seen = await locator.evaluate((element) => {
    let alpha = 1;
    let ground = null;
    for (let at = element; at; at = at.parentElement) {
      const style = getComputedStyle(at);
      alpha *= Number(style.opacity);
      const fill = style.backgroundColor;
      if (!ground && fill !== "transparent" && !fill.endsWith(", 0)")) ground = fill;
    }
    return { color: getComputedStyle(element).color, alpha, ground };
  });
  const ground = parse(seen.ground ?? "rgb(255, 255, 255)");
  return ratio(over(parse(seen.color), ground, seen.alpha), ground);
};

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
      await new Promise((done) => setTimeout(done, 100));
    }
  }
  const invited = await new Deno.Command(binary, {
    args: [
      "passkey",
      "invite",
      "keyboard",
      "--ledger",
      "default=delete",
      "--browse",
      "derive",
      ...homes,
    ],
  }).output();
  const said = new TextDecoder().decode(invited.stdout);
  assert.ok(invited.success, said + new TextDecoder().decode(invited.stderr));
  const invite = said.match(/http:\/\/\S*#invite=\S+/)[0];

  const context = await browser.newContext({
    viewport: { width: 1280, height: 720 },
    bypassCSP: true,
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
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

  const link = page.locator('a.navlink[href="/queue"]');
  const badge = link.locator(".badge-depth");
  const named = async () => {
    await cdp.send("Accessibility.enable");
    const { nodes } = await cdp.send("Accessibility.getFullAXTree");
    const node = nodes.find((n) =>
      n.role?.value === "link" && /^Queue/.test(n.name?.value ?? "")
    );
    return {
      name: node?.name?.value ?? "",
      description: node?.description?.value ?? "",
    };
  };

  // An idle, empty queue: the dimmest the badge gets.
  await page.goto(`${base}/l/default`);
  await badge.waitFor();
  check(
    (await badge.getAttribute("class")).includes("empty"),
    "an idle queue's badge is empty",
  );
  for (const theme of ["light", "dark", "high-contrast"]) {
    await page.goto(`${base}/l/default?theme=${theme}`);
    await badge.waitFor();
    const floor = theme === "high-contrast" ? 7 : 4.5;
    for (const part of [".serious", ".other"]) {
      const got = await contrast(badge.locator(part));
      check(got >= floor, `${theme} ${part} is ${got.toFixed(2)}:1 (floor ${floor}:1)`);
    }
  }

  const idle = await named();
  console.log(`name: ${JSON.stringify(idle.name)}\ndescription: ${JSON.stringify(idle.description)}`);
  check(
    /^Queue\s*, 0 serious\s*, \+0 other$/.test(idle.name),
    `the idle link's name says what its numbers are: ${JSON.stringify(idle.name)}`,
  );
  check(
    /serious findings? waiting for a decision/.test(idle.description),
    `the state sentence is the link's description: ${JSON.stringify(idle.description)}`,
  );

  // A request a pass took and never settled, with nothing in flight: stuck, by ledger #737.
  await Deno.mkdir(`${scratch}/spaces/reviews/.processing`, { recursive: true });
  await Deno.writeTextFile(`${scratch}/spaces/reviews/.processing/lost.tuple`, "()\n");
  await page.goto(`${base}/l/default`);
  await badge.waitFor();
  const stuckClass = await badge.getAttribute("class");
  check(stuckClass.includes("stuck"), `a lost request is stuck: ${stuckClass}`);
  const stuck = await named();
  console.log(`name: ${JSON.stringify(stuck.name)}\ndescription: ${JSON.stringify(stuck.description)}`);
  const word = badge.locator(".state");
  check(
    (await word.count()) === 1 && await word.isVisible() && (await word.innerText()) === "stuck",
    "a stuck queue shows the word",
  );
  check(/stuck/.test(stuck.name), `and the link's name says it: ${JSON.stringify(stuck.name)}`);
  check(
    /never finished/.test(stuck.description),
    `and the description says why: ${JSON.stringify(stuck.description)}`,
  );
  await Deno.remove(`${scratch}/spaces/reviews/.processing/lost.tuple`);

  // Item 3 and 8's page: every signed-in page axe was shown, in each theme and at both widths.
  await page.goto(`${base}/l/default`);
  await page.fill("#file-content", "A scratch item\n\nIts body.");
  await page.locator("form.file button[type=submit]").first().focus();
  await page.keyboard.press("Enter");
  await page.locator("#ledger").getByText("A scratch item").first().waitFor();
  const swapped = await page.evaluate(() => {
    const el = document.activeElement;
    return { tag: el.tagName, inRegion: !!el.closest("#ledger") };
  });
  check(
    swapped.inRegion,
    `focus after filing, a swap of #ledger, is in the region: ${JSON.stringify(swapped)}`,
  );

  for (const theme of ["auto", "light", "dark", "high-contrast"]) {
    for (const width of [1280, 320]) {
      await page.setViewportSize({ width, height: 720 });
      for (const path of ["/l/default", "/l/default/item/1", "/queue", "/sparql"]) {
        await page.goto(`${base}${path}?theme=${theme}`);
        await badge.waitFor();
        await page.evaluate(() =>
          document.querySelectorAll("details").forEach((d) => (d.open = true))
        );
        const { violations } = await new AxeBuilder({ page })
          .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
          .analyze();
        const where = `${path} ${theme} ${width}`;
        check(
          violations.length === 0,
          `axe ${where}: ${violations.map((v) => v.id).join(" ") || "no violation"}`,
        );
        const scroll = await page.evaluate(() => document.documentElement.scrollWidth);
        check(scroll <= width, `${where}: scrollWidth ${scroll}`);
      }
    }
  }
  await page.setViewportSize({ width: 1280, height: 720 });

  // Item 4: the SPARQL example takes focus when it scrolls.
  await page.setViewportSize({ width: 320, height: 640 });
  await page.goto(`${base}/sparql`);
  const example = page.locator(".cross-graph-text");
  await example.evaluate((el) => {
    for (let at = el.parentElement; at; at = at.parentElement) {
      if (at.tagName === "DETAILS") at.open = true;
    }
  });
  await example.focus();
  check(
    await example.evaluate((el) => document.activeElement === el && el.scrollWidth > el.clientWidth),
    "the cross-graph example scrolls at 320 px and takes focus",
  );
  await page.setViewportSize({ width: 1280, height: 720 });

  // Item 8: the ledger's forms with JavaScript off, on the same sign-in.
  const off = await browser.newContext({
    javaScriptEnabled: false,
    storageState: await context.storageState(),
    viewport: { width: 1280, height: 720 },
  });
  const plain = await off.newPage();
  plain.on("request", async (request) => {
    if (request.method() === "POST") {
      const { origin, "sec-fetch-site": site } = await request.allHeaders();
      console.log(`a plain form's POST: Origin ${origin}, Sec-Fetch-Site ${site}`);
    }
  });
  const open = await plain.goto(`${base}/l/default`);
  check(open.status() === 200, `with no script the ledger answers ${open.status()}`);
  const canFile = await plain.locator("#file-content").count();
  console.log(`with no script the ledger's filing form is drawn: ${canFile}`);
  if (canFile) {
    await plain.fill("#file-content", "Filed with no script\n\nIts body.");
    const [filed] = await Promise.all([
      plain.waitForNavigation(),
      plain.locator("form.file button[type=submit]").first().click(),
    ]);
    check(filed.status() === 200, `filing answers ${filed.status()} at ${plain.url()}`);
    check(
      (await plain.locator("body").innerText()).includes("Filed with no script"),
      "and the answer is a page holding the new item",
    );

    await plain.goto(`${base}/l/default/item/2`);
    for (const [action, then] of [["close", "closed"], ["reopen", "open"]]) {
      const form = plain.locator(`form:has(input[name="_action"][value="${action}"])`).first();
      check((await form.count()) === 1, `the item page has a ${action} form`);
      if (await form.count()) {
        const [answer] = await Promise.all([
          plain.waitForNavigation(),
          form.locator("button[type=submit], button:not([type])").first().click(),
        ]);
        check(answer.status() === 200, `${action} answers ${answer.status()} at ${plain.url()}`);
        check(
          (await plain.locator("body").innerText()).toLowerCase().includes(then),
          `and the page says ${then}`,
        );
      }
    }
  } else {
    check(false, "the signed-in session does not reach a page with JavaScript off");
  }
  await off.close();

  check(errors.length === 0, `no page error: ${errors.join("; ")}`);
} finally {
  await browser.close();
  try {
    server.kill();
  } catch {
    // It had already gone: the failure above says why.
  }
  await server.status;
  await Deno.remove(scratch, { recursive: true });
}
if (failures.length) {
  console.log(`\n${failures.length} failed:\n${failures.join("\n")}`);
  Deno.exit(1);
}
console.log("all held");
