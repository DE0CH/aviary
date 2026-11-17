// The operator page's state colours (src/ops_page.html), run in Node with a stub document: `node --test` (ci.yml, job test).
//
// The flight deck's colours (Deyao, 2026-11-17): red ("problem") is a service lost right now; amber ("watch") is still
// serving but degraded, or an error that is over; magenta ("blind") is something this page could not read; blue
// ("warming") is something that finishes by itself.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const html = readFileSync(new URL("../src/ops_page.html", import.meta.url), "utf8");
const css = html.slice(html.indexOf("<style>"), html.indexOf("</style>"));
const script = html
  .slice(html.indexOf("<script>") + "<script>".length, html.lastIndexOf("</script>"))
  .replace("{{TZ_JSON}}", '"UTC"')
  .replace("{{PRODUCT_JSON}}", '"Example AI"');

/** the page's script in a context of its own, its elements kept by id; no snapshot is fetched */
function page() {
  const els = {};
  const el = (id) => (els[id] ||= { className: "", textContent: "", innerHTML: "" });
  const ctx = vm.createContext({
    document: { getElementById: el, addEventListener() {}, hidden: false, body: { className: "" } },
    location: { hash: "", search: "", pathname: "/ops/" },
    history: { replaceState() {} },
    fetch: () => new Promise(() => {}),
    setInterval: () => 0,
    Intl,
    console,
  });
  vm.runInContext(script, ctx);
  return { ctx, el };
}

const NOW = new Date().toISOString();
const base = () => ({ at: NOW, errors: {}, health: { state: "ok", findings: [] }, recentMinutes: 15, fabric: "gvisor-aws" });
const pod = (name, ready) => ({ name: "pigeon-abc-" + name, ready, phase: ready ? "Running" : "Pending", ageS: 60, restarts: 0 });
const dep = (state, ready) => ({
  available: true,
  deployments: [{ name: "pigeon", state, desired: 2, ready, rolling: false, pods: [pod("a", ready > 0), pod("b", false)] }],
});

test("the stylesheet: each state has its own colour", () => {
  assert.match(css, /\.s-warming\{color:var\(--info\)/, "warming up is blue, not the amber of a caution");
  assert.match(css, /\.s-watch\{color:var\(--warm\)/);
  assert.match(css, /\.s-blind\{color:var\(--blind\)/);
  assert.match(css, /\.s-problem\{color:var\(--bad\)/);
  assert.match(css, /\.err\{color:var\(--blind\)/, "a part that could not be read is blind, not red");
  const root = css.slice(css.indexOf(":root{"), css.indexOf("}", css.indexOf(":root{")));
  const v = (name) => root.match(new RegExp(name + ":(#[0-9a-f]{6})"))[1];
  assert.equal(new Set([v("--ok"), v("--warm"), v("--bad"), v("--info"), v("--blind")]).size, 5);
});

test("a Deployment short of pods whose others serve is amber; with none ready it is red", () => {
  const { ctx } = page();
  const short = ctx.renderNow({ ...base(), service: dep("watch", 1) });
  assert.match(short, /chip s-watch">1 \/ 2 ready/);
  assert.match(short, /dot watch/, "its pod that is not ready is amber too");
  assert.doesNotMatch(short, /s-problem|dot problem/);
  const down = ctx.renderNow({ ...base(), service: dep("problem", 0) });
  assert.match(down, /chip s-problem">0 \/ 2 ready/);
  assert.match(down, /dot problem/);
});

test("a host not answering is red with a sandbox on it, amber when empty; booting is blue", () => {
  const { ctx } = page();
  const host = (o) => ({ name: "aws-1", status: "active", reachable: false, sandboxes: 0, launchedAt: NOW, ...o });
  assert.equal(ctx.hostLevel(host({ sandboxes: 2 })), "problem");
  assert.equal(ctx.hostLevel(host({})), "watch");
  assert.equal(ctx.hostLevel(host({ status: "booting" })), "warming");
  assert.equal(ctx.hostLevel(host({ reachable: true })), "ok");
});

test("a hermit replica down beside a healthy one is amber; with none healthy it is red", () => {
  const { ctx } = page();
  const silent = { pod: "hermit-a", reporting: false, seenAt: NOW };
  const beside = ctx.renderNow({ ...base(), replicas: [silent, { pod: "hermit-b" }] });
  assert.match(beside, /chip s-watch">stopped reporting/);
  assert.doesNotMatch(beside, /s-problem/);
  const alone = ctx.renderNow({ ...base(), replicas: [silent] });
  assert.match(alone, /chip s-problem">stopped reporting/);
});

test("blind: the status, its finding and a part that could not be read", () => {
  const { ctx, el } = page();
  const d = {
    ...base(),
    errors: { budget: "timeout" },
    health: { state: "blind", findings: [{ level: "blind", about: "page", text: "Could not read budget: timeout" }] },
  };
  ctx.render(d);
  assert.equal(el("verdict").className, "pill s-blind");
  assert.equal(el("verdict").textContent, "Can't see · 1");
  const now = el("now-body").innerHTML;
  assert.match(now, /<span class="dot blind"><\/span><span class="lv blind">Blind<\/span>/);
  assert.match(now, /class="err">Could not read this part: timeout/);
  assert.match(html, /<span class="meta blindc" id="stale">/, "a page that stopped updating is blind");
});

test("errors that are over are amber, and a busy sandbox is not a caution", () => {
  const { ctx } = page();
  const d = {
    ...base(),
    recent: { list: [{ at: NOW, what: "sandbox start failed", detail: "pool_full" }], blocked: 0 },
    catalogue: { refused: "admin not in users" },
    sandboxes: { list: [{ owner: "a@example.com", status: "ready", reporting: true, busy: true, ageS: 5, idleS: 1 }] },
  };
  const out = ctx.renderNow(d);
  assert.match(out, /<span class="main warmc">sandbox start failed<\/span>/);
  assert.match(out, /<span class="main warmc">Staff catalogue refused<\/span>/);
  assert.match(out, /chip s-none">busy/);
  assert.doesNotMatch(out, /class="main bad"/);
});
