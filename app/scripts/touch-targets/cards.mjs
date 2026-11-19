// The question cards in the built app, live (Deyao, 2026-11-19: a card drawn twice and a list that scrolled wrongly,
// both from one row pushed live with no turn):
//
//   node scripts/touch-targets/cards.mjs DIST [OUT]
//
// Serves the web export DIST with the stand-in for pigeon, the deployment's cards switched on, and opens conversation
// 6 (a running turn) on a 390 x 844 touch phone in Chromium. The stand-in replays what drew one question twice: the
// `ask` call runs, the card's row (no turn) opens and is answered, the `ask` call completes, and the app loads the
// timeline again (content.mjs cardPushes). The page must then show the question once, answered and not read yet (its
// answer printed no marker); still once after the conversation is scrolled to its top and back; with no page error.
// OUT (optional) gets a screenshot. Exit 0: as it should; 1: not.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { startStandIn } from "./stand-in.mjs";
import * as content from "./content.mjs";

const require = createRequire(path.join(path.dirname(new URL(import.meta.url).pathname), "../../paseo/packages/app/package.json"));
const { chromium } = require("playwright");

const [dist, out] = process.argv.slice(2);
if (!dist || !fs.existsSync(path.join(dist, "index.html"))) {
  console.error("usage: cards.mjs DIST [OUT]   (DIST: the web export, with index.html)");
  process.exit(2);
}
if (out) fs.mkdirSync(out, { recursive: true });
content.options.personCards = true;
content.options.cardsLive = true;

const standIn = await startStandIn({ appDir: dist });
const browser = await chromium.launch();
const problems = [];
try {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, locale: "zh-HK" });
  const page = await context.newPage();
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("console", (m) => {
    if (m.type() === "error" && /same key|two children with the same key/i.test(m.text())) problems.push(`console: ${m.text()}`);
  });
  await page.goto(`${standIn.url}/h/srv_conversations/workspace/wks_00000006-0000-4000-8000-000000000000`);
  // the pushes and the reload end about 2.5 s after the timeline loads
  await page.waitForTimeout(6000);
  const cards = async () =>
    page.evaluate(() => [...document.querySelectorAll('[data-testid^="person-card-"]')].map((e) => e.getAttribute("data-testid")));
  const scroll = (f) =>
    page.evaluate((frac) => {
      const [list] = [...document.querySelectorAll("div")]
        .filter((d) => d.scrollHeight > d.clientHeight + 40 && /(auto|scroll)/.test(getComputedStyle(d).overflowY))
        .sort((a, b) => b.clientHeight - a.clientHeight);
      if (list) list.scrollTop = (list.scrollHeight - list.clientHeight) * frac;
    }, f);
  const want = JSON.stringify(["person-card-unread"]);
  const first = await cards();
  if (JSON.stringify(first) !== want) problems.push(`after the pushes: ${JSON.stringify(first)}, want ${want}`);
  await scroll(0);
  await page.waitForTimeout(800);
  await scroll(1);
  await page.waitForTimeout(800);
  const after = await cards();
  if (JSON.stringify(after) !== want) problems.push(`after scrolling: ${JSON.stringify(after)}, want ${want}`);
  const visible = await page.locator('[data-testid="person-card-unread"]').first().isVisible().catch(() => false);
  if (!visible) problems.push("the card is not on screen at the conversation's end");
  if (out) await page.screenshot({ path: path.join(out, "cards.png") });
  await context.close();
} finally {
  await browser.close();
  await standIn.close();
}
if (problems.length) {
  console.error(problems.join("\n"));
  process.exit(1);
}
console.log("the question once, answered and not read yet; still once after scrolling");
