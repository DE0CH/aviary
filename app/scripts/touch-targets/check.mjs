// Every control of the phone layout is at least a finger's 44 px (Deyao, 2026-11-18; docs/paseo-ui.md "Touch targets").
//
//   node scripts/touch-targets/check.mjs DIST [OUT]
//
// Serves the web export DIST with the stand-in for pigeon (stand-in.mjs), opens every screen and state on a 390 x 844
// touch phone in Chromium, and measures every control on the screen: anything a finger presses (buttons, links,
// inputs, switches, menu rows) whose box is under 44 px either way fails the check, by screen, label and size.
// An inline link inside running text is exempt (WCAG 2.5.8), as are full-screen backdrops. OUT (optional) gets a
// screenshot of each screen with the small controls outlined. Exit 0: all big enough; 1: some are not; 2: setup.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { startStandIn } from "./stand-in.mjs";

const require = createRequire(path.join(path.dirname(new URL(import.meta.url).pathname), "../../paseo/packages/app/package.json"));
const { chromium } = require("playwright");

const MIN = 44;
const [dist, out] = process.argv.slice(2);
if (!dist || !fs.existsSync(path.join(dist, "index.html"))) {
  console.error("usage: check.mjs DIST [OUT]   (DIST: the web export, with index.html)");
  process.exit(2);
}
if (out) fs.mkdirSync(out, { recursive: true });

const workspace = (n) => `/h/srv_conversations/workspace/wks_0000000${n}-0000-4000-8000-000000000000`;

// One entry per screen or state: where it starts and what a person does to get there.
const STATES = [
  { name: "new chat", at: "/new" },
  { name: "new chat, typing", at: "/new", run: (p) => p.type("幫我整理今日嘅會議紀錄。\n第二行\n第三行") },
  { name: "the / list", at: workspace(1), run: (p) => p.type("/") },
  { name: "conversation, end", at: workspace(1) },
  { name: "conversation, start", at: workspace(1), run: (p) => p.scroll(0) },
  { name: "conversation, middle", at: workspace(1), run: (p) => p.scroll(0.4) },
  { name: "conversation, sent files", at: workspace(1), run: (p) => p.scroll(0.7) },
  { name: "steps open", at: workspace(1), run: async (p) => { await p.scroll(0); await p.tap('[data-testid="turn-summary"] >> nth=0'); await p.scroll(0.12); } },
  { name: "tool list", at: workspace(1), run: async (p) => { await p.scroll(0); await p.tap('[data-testid="turn-summary"] >> nth=0'); await p.scroll(0.12); await p.tap("text=/^編輯了 1 個檔案/"); } },
  { name: "tool details", at: workspace(1), run: async (p) => { await p.scroll(0); await p.tap('[data-testid="turn-summary"] >> nth=0'); await p.scroll(0.12); await p.tap("text=/^編輯了 1 個檔案/"); await p.tap("text=ls uploads"); } },
  { name: "picture viewer", at: workspace(1), run: async (p) => { await p.scroll(0); await p.tap("img >> nth=0"); } },
  { name: "conversation menu", at: workspace(1), run: (p) => p.tap('[data-testid="workspace-header-menu-trigger"]') },
  { name: "rename", at: workspace(1), run: async (p) => { await p.tap('[data-testid="workspace-header-menu-trigger"]'); await p.tap('[data-testid="conversation-menu-rename"]'); } },
  { name: "share", at: workspace(1), run: async (p) => { await p.tap('[data-testid="workspace-header-menu-trigger"]'); await p.tap('[data-testid="conversation-menu-share"]'); } },
  { name: "model menu", at: workspace(1), run: (p) => p.tap('[data-testid="yyc-model-menu"]') },
  { name: "model switch question", at: workspace(1), run: async (p) => { await p.tap('[data-testid="yyc-model-menu"]'); await p.tap('[data-testid="yyc-model-xiaomi/mimo-v2.6-pro"]'); await p.wait(1500); } },
  { name: "attachments", at: workspace(1), run: (p) => p.tap('[data-testid="message-input-attach-button"]') },
  { name: "sidebar", at: workspace(1), run: (p) => p.tap('[data-testid="menu-button"]') },
  { name: "sidebar, a conversation's menu", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid^="sidebar-workspace-kebab-"] >> nth=1'); } },
  { name: "sidebar, more", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid^="sidebar-project-show-more"]'); await p.scroll(1); } },
  { name: "search", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-search"]'); } },
  { name: "recently deleted", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-sessions"]'); } },
  { name: "settings", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-settings"]'); } },
  { name: "settings, general", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-settings"]'); await p.tap("text=通用"); } },
  { name: "settings, appearance", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-settings"]'); await p.tap("text=外觀"); } },
  { name: "settings, theme menu", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-settings"]'); await p.tap("text=外觀"); await p.tap("text=跟隨系統"); } },
  { name: "settings, chat", at: workspace(1), run: async (p) => { await p.tap('[data-testid="menu-button"]'); await p.tap('[data-testid="sidebar-settings"]'); await p.tap("text=聊天"); } },
  { name: "a reply running", at: workspace(2) },
  { name: "schedules chip", at: workspace(3) },
  { name: "schedules", at: workspace(3), run: (p) => p.tap('[data-testid="schedules-chip"]') },
  { name: "activity chip", at: workspace(4) },
  { name: "activity", at: workspace(4), run: (p) => p.tap('[data-testid="activity-chip"]') },
  { name: "a question from the AI", at: workspace(5), run: (p) => p.scroll(1) },
  { name: "a message waiting for the network", at: workspace(1), run: async (p) => { await p.type("多謝！"); await p.tap('[aria-label="傳送"]'); await p.wait(6500); } },
  { name: "a shared conversation", at: "/s/sh1" },
  { name: "signed out", at: "/new", signedOut: true },
  { name: "no server", at: "/new", noServer: true, run: (p) => p.wait(4000) },
  { name: "message box full screen", at: workspace(1), run: async (p) => { await p.type("第一行\n第二行\n第三行\n第四行"); await p.tap('[aria-label="全屏"]'); } },
];

// In the page: the controls on screen and their boxes, the small ones outlined and numbered.
const MEASURE = ({ min, mark }) => {
  const selector = 'button, a[href], input:not([type=hidden]), textarea, select, [role=button], [role=link], [role=switch], [role=tab], [role=menuitem], [role=checkbox], [role=radio], [role=option], [tabindex="0"]';
  const shown = [...document.querySelectorAll(selector)].filter((el) => {
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    if (r.width < 1 || r.height < 1 || cs.visibility === "hidden" || Number(cs.opacity) === 0) return false;
    if (r.bottom <= 0 || r.top >= innerHeight || r.right <= 0 || r.left >= innerWidth) return false;
    const x = Math.min(Math.max(r.left + r.width / 2, 0), innerWidth - 1);
    const y = Math.min(Math.max(r.top + r.height / 2, 0), innerHeight - 1);
    const top = document.elementFromPoint(x, y);
    return top && (el.contains(top) || top.contains(el));
  });
  const sameBox = (a, b) => {
    const r = a.getBoundingClientRect();
    const s = b.getBoundingClientRect();
    return Math.abs(r.width - s.width) < 2 && Math.abs(r.height - s.height) < 2;
  };
  // a wrapper the size of the control inside it is one target; a control inside a bigger one is a target of its own
  const targets = shown
    .filter((el) => !shown.some((o) => o !== el && el.contains(o) && sameBox(el, o)))
    .filter((el) => { const r = el.getBoundingClientRect(); return !(r.width >= innerWidth - 2 && r.height >= innerHeight * 0.6); });
  let n = 0;
  return targets.map((el) => {
    const r = el.getBoundingClientRect();
    const inline = el.tagName === "A" && getComputedStyle(el).display.startsWith("inline");
    const small = !inline && Math.min(r.width, r.height) < min - 0.5;
    const label = (el.getAttribute("aria-label") || el.innerText || el.getAttribute("placeholder") || el.getAttribute("data-testid") || el.tagName)
      .trim().replace(/\s+/g, " ").slice(0, 48);
    if (small && mark) {
      n += 1;
      const box = document.createElement("div");
      Object.assign(box.style, { position: "fixed", left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px`, outline: "2px solid #e00", zIndex: 2147483647, pointerEvents: "none" });
      box.textContent = String(n);
      Object.assign(box.style, { color: "#e00", font: "600 10px system-ui" });
      document.body.appendChild(box);
    }
    return { label, width: Math.round(r.width), height: Math.round(r.height), small };
  });
};

async function walk(browser, base, state) {
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, locale: "zh-HK",
    extraHTTPHeaders: state.signedOut ? { "x-signed-out": "1" } : undefined,
  });
  if (state.noServer) await context.routeWebSocket(/paseo-ws/, (ws) => ws.close());
  const page = await context.newPage();
  const problems = [];
  const helpers = {
    wait: (ms) => page.waitForTimeout(ms),
    tap: async (selector) => {
      await page.locator(selector).first().click({ timeout: 8000 });
      await page.waitForTimeout(900);
    },
    type: async (text) => {
      const box = page.locator("textarea").first();
      await box.click();
      await box.fill(text);
      await page.waitForTimeout(700);
    },
    scroll: async (fraction) => {
      await page.evaluate((f) => {
        const [list] = [...document.querySelectorAll("div")]
          .filter((d) => d.scrollHeight > d.clientHeight + 40 && /(auto|scroll)/.test(getComputedStyle(d).overflowY))
          .sort((a, b) => b.clientHeight - a.clientHeight);
        if (list) list.scrollTop = (list.scrollHeight - list.clientHeight) * f;
      }, fraction);
      await page.waitForTimeout(900);
    },
  };
  try {
    await page.goto(base + state.at);
    await page.waitForTimeout(5000);
    if (state.run) await state.run(helpers);
    await page.waitForTimeout(700);
  } catch (error) {
    // a screen that could not be reached is a failure too: a control nobody can measure is not known to be big enough
    problems.push(`could not reach it: ${String(error.message ?? error).split("\n")[0]}`);
  }
  const controls = await page.evaluate(MEASURE, { min: MIN, mark: Boolean(out) });
  if (out) await page.screenshot({ path: path.join(out, `${STATES.indexOf(state) + 1}-${state.name.replace(/[^a-z0-9]+/gi, "-")}.png`) });
  await context.close();
  if (controls.length === 0 && problems.length === 0) problems.push("no controls on the screen (it did not load)");
  for (const c of controls.filter((c) => c.small)) problems.push(`${c.label}: ${c.width} x ${c.height}`);
  return { controls: controls.length, problems };
}

const standIn = await startStandIn({ appDir: dist });
const browser = await chromium.launch();
let failed = 0;
try {
  for (const state of STATES) {
    const { controls, problems } = await walk(browser, standIn.url, state);
    if (problems.length === 0) {
      console.log(`ok   ${state.name} (${controls} controls)`);
      continue;
    }
    failed += 1;
    console.log(`FAIL ${state.name}`);
    for (const p of problems) console.log(`       ${p}`);
  }
} finally {
  await browser.close();
  await standIn.close();
}
console.log(failed ? `${failed} of ${STATES.length} screens have controls under ${MIN} px` : `all ${STATES.length} screens: every control is at least ${MIN} px`);
process.exit(failed ? 1 : 0);
