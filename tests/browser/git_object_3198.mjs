// cargo run --locked --example axe_site -- target/axe-site
// deno run -A tests/browser/git_object_3198.mjs
// Static HTTP fixtures; no network publication.
import { chromium } from "npm:playwright@1.64.0";
import { AxeBuilder } from "npm:@axe-core/playwright@4.13.0";
import assert from "node:assert/strict";
const root = Deno.args[0] ?? "target/axe-site";
const types = JSON.parse(await Deno.readTextFile(`${root}/_types.json`));
const server = Deno.serve({ port: 0, hostname: "127.0.0.1", onListen() {} }, async request => {
 const path = new URL(request.url).pathname;
 if (!(path in types)) return new Response("Not found", {status:404});
 return new Response(await Deno.readFile(`${root}${path}`), {headers:{"Content-Type": types[path]}});
});
const browser = await chromium.launch();
let checks=0, axes=0;
try {
 console.log(`Chromium ${browser.version()}`);
 for(const javaScriptEnabled of [true,false]) for(const width of [1280,320]) for(const colorScheme of ["light","dark"]) for(const theme of ["auto","light","dark","high-contrast"]) {
  const context=await browser.newContext({viewport:{width,height:900},colorScheme,javaScriptEnabled});
  const page=await context.newPage(); const errors=[];
  page.on("pageerror", e=>errors.push(String(e)));
  page.on("console", m=>{if(m.type()==="error") errors.push(m.text());});
  for(const file of ["linked-item","git-object"]) {
   await page.goto(`http://127.0.0.1:${server.addr.port}/${file}.html`);
   await page.evaluate(t=>document.documentElement.dataset.theme=t,theme);
   if(file==="linked-item") {
    const links=page.locator(".links a.iri"); assert.equal(await links.count(),2);
    for(const link of await links.all()) {
     assert.match(await link.innerText(),/^urn:git:(commit|blob):[0-9a-f]{40}$/);
     assert.match(await link.getAttribute("href"),/^https:\/\/code.example\/(commit|blob)\//);
    }
    const reached=new Set();
    for(let i=0;i<100&&reached.size<2;i++) {
     await page.keyboard.press("Tab");
     const text=await page.evaluate(()=>document.activeElement?.matches(".links a.iri")?document.activeElement.textContent:null);
     if(text) reached.add(text);
    }
    assert.equal(reached.size,2,"both Git links are keyboard stops");
   } else {
    assert.match(await page.locator("pre.graph").innerText(),/<code> & evidence/);
    const reached=await page.locator("pre.graph").evaluate(async node=>{
     node.focus(); return document.activeElement===node;
    });
    assert.equal(reached,true);
    assert.equal(await page.getByRole("link",{name:"Browse fixture (current working tree)",exact:true}).count(),1);
   }
   assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),`no overflow ${file} ${width} ${theme}`);
   if(javaScriptEnabled) {
    const result=await new AxeBuilder({page}).withTags(["wcag2a","wcag2aa","wcag21a","wcag21aa","wcag22aa"]).analyze();
    assert.deepEqual(result.violations,[],JSON.stringify(result.violations)); axes++;
   }
   assert.deepEqual(errors,[]);
   checks++;
   console.log(`ok ${file} script=${javaScriptEnabled} width=${width} system=${colorScheme} theme=${theme}`);
  }
  await context.close();
 }
 console.log(`${checks} page checks; ${axes} axe checks; no violations`);
} finally {await browser.close(); await server.shutdown();}
