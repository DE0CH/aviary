// Screenshots of the UI against dev/mock-server.mjs (phone 390x844 and desktop 1440x900), for review.
//   PLAYWRIGHT_BROWSERS_PATH=… node dev/screens.mjs OUT_DIR [chromium|webkit] [share]   (share: only the sharing screens)
// Needs the `playwright` package resolvable (NODE_PATH) and a build in ../ui-dist.
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const pw = require("playwright");
const here = dirname(fileURLToPath(import.meta.url));
const out = process.argv[2] || "/tmp/ui-screens";
const engine = process.argv[3] || "chromium";
mkdirSync(out, { recursive: true });

function server(port, env) {
  const p = spawn(process.execPath, [join(here, "mock-server.mjs"), String(port)], { env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "inherit"] });
  return new Promise((resolve) => p.stdout.once("data", () => resolve(p)));
}

const browser = await pw[engine].launch();
const sizes = { phone: { width: 390, height: 844, isMobile: engine === "chromium", hasTouch: true, deviceScaleFactor: 2 }, desktop: { width: 1440, height: 900 } };

async function shoot(name, size, url, fn, opts = {}) {
  const ctx = await browser.newContext({ viewport: { width: sizes[size].width, height: sizes[size].height }, ...(size === "phone" ? { isMobile: sizes.phone.isMobile, hasTouch: true, deviceScaleFactor: 2 } : {}), colorScheme: opts.dark ? "dark" : "light", locale: "zh-HK" });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.error(`[${name}] page error: ${e.message}`));
  await page.goto(url);
  await page.waitForTimeout(500);
  if (fn) await fn(page);
  await page.screenshot({ path: join(out, `${engine}-${size}-${name}.png`) });
  await ctx.close();
}

const send = async (page, text) => {
  await page.fill("textarea", text);
  await page.click("button.send");
};

const only = process.argv[4] || "";

// sharing: the share sheet, a colleague's shared conversation (read-only), "continue conversation", a revoked link
const SHARED = "/s/MockShareColleague0001";
async function shareScreens(port, tag, locale = {}) {
  const sv = await server(port, { MOCK_SEED: "1", MOCK_SPEED: "4", ...locale });
  const b = `http://127.0.0.1:${port}`;
  for (const size of ["phone", "desktop"]) {
    await shoot(`share-sheet${tag}`, size, b + "/", async (p) => {
      await send(p, tag ? "Summarise today's coverage of financial services" : "總結今日香港傳媒對金融服務業的主要報道");
      await p.waitForSelector(".file-chip.made", { timeout: 20000 });
      await p.click(".topbar button[aria-label='" + (tag ? "Share" : "分享") + "']");
      await p.click(".sheet .primary-btn");
      await p.waitForSelector(".share-link");
      await p.waitForTimeout(200);
    });
    await shoot(`shared-page${tag}`, size, b + SHARED, async (p) => {
      await p.waitForSelector(".shared-note");
      await p.waitForTimeout(200);
    });
    if (!tag) {
      await shoot("shared-page-dark", size, b + SHARED, async (p) => {
        await p.waitForSelector(".shared-note");
      }, { dark: true });
      await shoot("continued", size, b + SHARED, async (p) => {
        await p.waitForSelector(".shared-note");
        await p.click(".continue-bar .primary-btn");
        await p.waitForURL(/\/c\//);
        await p.waitForSelector("textarea");
        await p.fill("textarea", "再按媒體分類，列出每篇報道的標題");
        await p.waitForTimeout(300);
      });
      await shoot("share-gone", size, b + "/s/RevokedLinkRevokedLink");
    }
  }
  sv.kill();
}
if (only === "share") {
  await shareScreens(5211, "");
  await shareScreens(5212, "-en", { MOCK_LOCALE: "en" });
  await browser.close();
  console.log(`screens in ${out}`);
  process.exit(0);
}

const s1 = await server(5201, { MOCK_SEED: "1", MOCK_SPEED: "4" });
const base = "http://127.0.0.1:5201";
for (const size of ["phone", "desktop"]) {
  await shoot("empty", size, base + "/");
  await shoot("empty-dark", size, base + "/", null, { dark: true });
  await shoot("streaming", size, base + "/", async (p) => {
    await send(p, "總結今日香港傳媒對金融服務業的主要報道");
    await p.waitForSelector(".step");
    await p.waitForTimeout(300);
  });
  // a sandbox download the allowlist refuses: an ordinary failed step, nothing to approve
  await shoot("sandbox-denied", size, base + "/", async (p) => {
    await send(p, "總結今日香港傳媒對金融服務業的主要報道");
    await p.waitForSelector(".step.is-error", { timeout: 20000 });
    await p.click(".step.is-error .step-head");
    await p.waitForTimeout(200);
  });
  await shoot("done-expanded", size, base + "/", async (p) => {
    await send(p, "總結今日香港傳媒對金融服務業的主要報道");
    await p.waitForSelector(".file-chip.made", { timeout: 20000 });
    await p.click(".step-head >> nth=0");
    await p.waitForTimeout(200);
  });
  await shoot("done-dark", size, base + "/", async (p) => {
    await send(p, "總結今日香港傳媒對金融服務業的主要報道");
    await p.waitForSelector(".file-chip.made", { timeout: 20000 });
  }, { dark: true });
  // a conversation an older version stored with a confirmation in it: nothing is shown for it
  await shoot("legacy-confirmation", size, base + "/", async (p) => {
    if (size === "phone") {
      await p.click("button[aria-label='選單']");
      await p.waitForTimeout(300);
    }
    await p.click(".item-main:has-text('本週金融服務報道摘要')");
    await p.waitForSelector(".bubble");
    await p.waitForTimeout(300);
  });
}
await shoot("drawer", "phone", base + "/", async (p) => {
  await p.click("button[aria-label='選單']");
  await p.waitForTimeout(400);
});
await shoot("history", "desktop", base + "/", async (p) => {
  await p.click(".item-main >> nth=0");
  await p.waitForSelector(".bubble");
  await p.click(".item-more >> nth=0");
});
s1.kill();

const s2 = await server(5202, { MOCK_LOCALE: "en", MOCK_SEED: "1", MOCK_SPEED: "4" });
for (const size of ["phone", "desktop"])
  await shoot("english", size, "http://127.0.0.1:5202/", async (p) => {
    await send(p, "Summarise today's coverage of financial services");
    await p.waitForSelector(".file-chip.made", { timeout: 20000 });
  });
s2.kill();

const s3 = await server(5203, { MOCK_DENY: "1" });
await shoot("denied", "phone", "http://127.0.0.1:5203/");
s3.kill();

await browser.close();
console.log(`screens in ${out}`);
