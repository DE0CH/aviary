// The Android app on a foldable, on an emulator against the stand-in for pigeon (docs/paseo-ui.md "The Android app").
//
//   node scripts/android-app/emulator-test.mjs APK OUT
//
// APK: a build of the app whose server (YYC_SITE) is http://127.0.0.1:8787, the stand-in this script starts there
// (scripts/touch-targets/stand-in.mjs) and hands to the emulator with `adb reverse`. Env: APP_PACKAGE (the build's
// package, also its URL scheme). The emulator must already be running (the workflow's android-emulator-runner).
//
// A person's path through what the iPhone app does for comfort, on the Galaxy Z Fold's two screens (Samsung's Fold7
// figures: the cover 1080 x 2520, the main screen 1968 x 2184, both at density 420; switching between them is the same
// configuration change a real fold sends): sign in, open a conversation, the message box rides on the keyboard, a
// flick down on the conversation puts the keyboard away, a fold and an unfold keep the app (same process), the
// conversation and the typed text, the main screen docks the sidebar, the app turns to landscape there; and once on
// the emulator's real hinge (`adb emu fold` / `unfold`). OUT gets a screenshot per step, a screen recording of the
// keyboard coming and going, each step's UI dump and results.json. Exit 0: every check passed; 1: some failed
// (a GitHub annotation per failure); 2: setup.
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { startStandIn } from "../touch-targets/stand-in.mjs";

const [apk, out] = process.argv.slice(2);
const pkg = process.env.APP_PACKAGE;
if (!apk || !out || !pkg || !fs.existsSync(apk)) {
  console.error("usage: APP_PACKAGE=<package> emulator-test.mjs APK OUT");
  process.exit(2);
}
fs.mkdirSync(out, { recursive: true });

const PORT = 8787;
const COVER = { name: "cover", size: "1080x2520", density: 420 };
const MAIN = { name: "main", size: "1968x2184", density: 420 };
const CONVERSATION = "季度銷售報告"; // the stand-in's first conversation (scripts/touch-targets/content.mjs)
const TYPED = "hello fold";

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
// every adb call has a time limit: a hung one fails its step instead of the job's two hours
const adb = (...args) =>
  execFileSync("adb", args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 180_000 });
const shell = (...args) => adb("shell", ...args);
const tryShell = (...args) => {
  try {
    return shell(...args);
  } catch (e) {
    return String(e.stdout ?? "") + String(e.stderr ?? e.message);
  }
};

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok: Boolean(ok), detail });
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` (${detail})` : ""}`);
  if (!ok) console.log(`::error title=android emulator test::${name}${detail ? `: ${detail}` : ""}`);
  return Boolean(ok);
}

// ---- the screen -------------------------------------------------------------------------------------------------

let step = 0;
function screenshot(name) {
  step += 1;
  const file = path.join(out, `${String(step).padStart(2, "0")}-${name}.png`);
  const raw = execFileSync("adb", ["exec-out", "screencap", "-p"], { maxBuffer: 64 * 1024 * 1024, timeout: 60_000 });
  // a profile with two screens (the Pixel Fold's) prints a warning ahead of the picture
  const start = raw.indexOf(Buffer.from([0x89, 0x50, 0x4e, 0x47]));
  fs.writeFileSync(file, start > 0 ? raw.subarray(start) : raw);
  return file;
}

/** The UI tree's nodes with their resource-id (React Native's testID), class, text and bounds in pixels. */
function dumpUi(label) {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const said = tryShell("uiautomator", "dump", "/sdcard/ui.xml");
    if (said.includes("dumped to")) {
      const xml = adb("exec-out", "cat", "/sdcard/ui.xml");
      if (label) fs.writeFileSync(path.join(out, `ui-${label}.xml`), xml);
      const nodes = [];
      for (const match of xml.matchAll(/<node ([^>]*?)\/?>/g)) {
        const attr = (name) => match[1].match(new RegExp(`${name}="([^"]*)"`))?.[1] ?? "";
        const b = attr("bounds").match(/\[(\d+),(\d+)\]\[(\d+),(\d+)\]/);
        if (!b) continue;
        nodes.push({
          id: attr("resource-id"),
          cls: attr("class"),
          text: attr("text").replace(/&#10;/g, "\n").replace(/&amp;/g, "&"),
          desc: attr("content-desc"),
          focused: attr("focused") === "true",
          box: { left: +b[1], top: +b[2], right: +b[3], bottom: +b[4] },
        });
      }
      return nodes;
    }
    // "could not get idle state": a spinner is still turning
    execFileSync("sleep", ["1"]);
  }
  return [];
}

const byId = (nodes, id) => nodes.find((n) => n.id === id || n.id.endsWith(`:id/${id}`));
const byText = (nodes, text) => nodes.find((n) => n.text === text || n.desc === text);
const within = (inner, outer) =>
  inner.box.left >= outer.box.left && inner.box.right <= outer.box.right &&
  inner.box.top >= outer.box.top && inner.box.bottom <= outer.box.bottom;
const centre = (n) => [Math.round((n.box.left + n.box.right) / 2), Math.round((n.box.top + n.box.bottom) / 2)];
const tap = (n) => shell("input", "tap", ...centre(n).map(String));

// Dialogs that are neither the app's nor the test's: the emulator's "… isn't responding" (its launcher, on a slow
// runner: Wait) and Chrome's first-run offers in the sign-in tab
const NUISANCE = /^(Wait|Use without an account|No thanks|No, thanks|Not now|Got it|Accept & continue|Don.t allow|Dismiss|Skip)$/;
function answerNuisance(nodes) {
  if (nodes.some((n) => n.text.includes("isn't responding"))) {
    const wait = byText(nodes, "Wait");
    if (wait) tap(wait);
    return true;
  }
  const button = nodes.find((n) => n.cls.endsWith("Button") && NUISANCE.test(n.text));
  if (button) tap(button);
  return Boolean(button);
}

async function waitFor(find, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let nodes = [];
  while (Date.now() < deadline) {
    nodes = dumpUi();
    const found = find(nodes);
    if (found) return found;
    if (answerNuisance(nodes)) await sleep(2000);
    await sleep(1000);
  }
  if (label) dumpUi(`timeout-${label}`);
  return null;
}

/** The message box (its root view) and the text field inside it. */
function composer(nodes) {
  const root = byId(nodes, "message-input-root");
  if (!root) return null;
  const field = nodes.find((n) => n.cls === "android.widget.EditText" && within(n, root));
  return field ? { root, field } : null;
}

/** The keyboard's area on screen from the window manager's insets (null when it is hidden). */
function keyboardFrame() {
  const dump = tryShell("dumpsys", "window", "displays");
  fs.writeFileSync(path.join(out, "last-insets.txt"), dump);
  for (const line of dump.split("\n")) {
    if (!/type=ime\b/.test(line)) continue;
    const frame = line.match(/frame=\[(\d+),(\d+)\]\[(\d+),(\d+)\]/);
    const visible = line.match(/visible=(true|false)/);
    if (frame && visible?.[1] === "true" && +frame[4] > +frame[2]) return { top: +frame[2], bottom: +frame[4] };
  }
  return null;
}

async function waitKeyboard(shown, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const frame = keyboardFrame();
    if (shown ? frame : !frame) {
      // the box moves with the keyboard's own animation: let it land
      await sleep(800);
      return shown ? keyboardFrame() ?? frame : true;
    }
    await sleep(250);
  }
  return shown ? null : false;
}

const appPid = () => tryShell("pidof", pkg).trim();

function setScreen({ size, density }) {
  shell("wm", "size", size);
  shell("wm", "density", String(density));
}

/** The message box sits on the keyboard: not covered by it, and not floating far above it. */
async function checkBoxOnKeyboard(posture) {
  const frame = await waitKeyboard(true);
  if (!check(`${posture}: the keyboard opens from the message box`, frame)) return;
  const nodes = dumpUi(`${posture}-keyboard`);
  const box = composer(nodes);
  if (!check(`${posture}: the message box is on screen with the keyboard up`, box)) return;
  const gap = frame.top - box.root.box.bottom;
  // above the keyboard (a pixel of rounding), and within the box's own margin and the toolbar's height of it
  check(`${posture}: the message box rides on the keyboard`, gap >= -2 && gap <= 160,
    `box bottom ${box.root.box.bottom}, keyboard top ${frame.top}, gap ${gap}px`);
}

// ---- the path ---------------------------------------------------------------------------------------------------

async function main() {
  const standIn = await startStandIn({
    appDir: path.join(path.dirname(new URL(import.meta.url).pathname), "../../paseo/packages/app/public"),
    port: PORT,
    appScheme: pkg,
  });
  try {
    adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
    // Chrome opens the sign-in (a Custom Tab): no first-run screens in the way
    tryShell("am", "set-debug-app", "--persistent", "com.android.chrome");
    tryShell("sh", "-c", "echo '_ --disable-fre --no-default-browser-check --no-first-run' > /data/local/tmp/chrome-command-line");
    shell("logcat", "-G", "32M"); // room for the whole run's log
    // a fresh emulator's Play Store updates itself and starves everything else (the launcher stops responding)
    tryShell("pm", "disable-user", "--user", "0", "com.android.vending");
    // the emulator has a hardware keyboard: show the on-screen one anyway, as on a phone
    shell("settings", "put", "secure", "show_ime_with_hard_keyboard", "1");
    shell("settings", "put", "system", "accelerometer_rotation", "0");
    shell("settings", "put", "system", "user_rotation", "0");
    adb("install", "-r", "-g", apk);

    // ---- folded: the cover screen ----
    setScreen(COVER);
    // let the freshly booted emulator settle (its launcher's "isn't responding" comes in the first minute)
    for (let i = 0; i < 10; i += 1) {
      answerNuisance(dumpUi());
      await sleep(3000);
    }
    // a freshly booted emulator sometimes drops the first start: ask again
    let signIn = null;
    for (let attempt = 0; attempt < 3 && !signIn; attempt += 1) {
      shell("am", "start", "-W", "-n", `${pkg}/.MainActivity`);
      signIn = await waitFor((n) => byId(n, "yyc-sign-in"), 60_000, attempt === 2 ? "sign-in" : undefined);
    }
    screenshot("cover-sign-in");
    if (!check("the app starts on its sign-in screen", signIn)) return;
    tap(signIn);
    const signedIn = await waitFor((n) => composer(n) || byId(n, "menu-button"), 90_000, "signed-in");
    screenshot("cover-signed-in");
    if (!check("sign-in returns to the app, signed in", signedIn)) return;
    const pid = appPid();

    const menu = await waitFor((n) => byId(n, "menu-button"), 20_000, "menu");
    if (menu) {
      tap(menu);
      const row = await waitFor((n) => byText(n, CONVERSATION), 20_000, "conversation-row");
      if (row) tap(row);
    }
    // open: the chat and its box, and the drawer gone (the chat is in the tree under an open drawer too)
    const opened = await waitFor((n) => byId(n, "agent-chat-scroll") && composer(n) && !byId(n, "sidebar-close"),
      30_000, "conversation");
    screenshot("cover-conversation");
    if (!check("a conversation opens from the conversation list", opened)) return;

    // the keyboard comes and goes, recorded
    const recording = spawn("adb", ["shell", "screenrecord", "--time-limit", "20", "/sdcard/keyboard.mp4"]);
    // its end, caught from the start: it may well be over before the steps below are
    const recorded = new Promise((done) => {
      recording.on("exit", done);
      recording.on("error", done);
    });
    await sleep(1000);
    tap(composer(dumpUi()).field);
    await checkBoxOnKeyboard("cover");
    shell("input", "text", TYPED.replace(/ /g, "%s"));
    await sleep(800);
    screenshot("cover-keyboard");
    const typed = composer(dumpUi("cover-typed"));
    check("typed text lands in the message box", typed?.field.text.includes(TYPED), typed?.field.text ?? "no box");

    // a flick down on the conversation puts the keyboard away (agent-stream/scroll-keyboard-dismiss)
    const list = byId(dumpUi(), "agent-chat-scroll");
    const keyboard = keyboardFrame();
    if (list && keyboard) {
      const x = Math.round((list.box.left + list.box.right) / 2);
      const top = list.box.top + 40;
      const bottom = Math.min(list.box.bottom, keyboard.top) - 40;
      shell("input", "swipe", String(x), String(top), String(x), String(bottom), "80");
    }
    check("a flick down on the conversation puts the keyboard away", await waitKeyboard(false));
    screenshot("cover-keyboard-away");
    await Promise.race([recorded, sleep(30_000)]);
    try {
      fs.writeFileSync(path.join(out, "keyboard-cover.mp4"),
        execFileSync("adb", ["exec-out", "cat", "/sdcard/keyboard.mp4"], { maxBuffer: 256 * 1024 * 1024, timeout: 120_000 }));
    } catch {
      console.log("::warning title=android emulator test::no screen recording");
    }

    // ---- unfold: the main screen, with the keyboard up ----
    tap(composer(dumpUi()).field);
    await waitKeyboard(true);
    setScreen(MAIN);
    await sleep(3000);
    screenshot("main-unfolded");
    check("unfolding keeps the app running (no restart)", appPid() === pid, `pid ${pid} → ${appPid()}`);
    const unfolded = dumpUi("main-unfolded");
    check("unfolding keeps the conversation and the typed text",
      byId(unfolded, "agent-chat-scroll") && composer(unfolded)?.field.text.includes(TYPED),
      composer(unfolded)?.field.text ?? "no box");
    // 1968 px at density 420 is 750 dp: a docked sidebar, no drawer (constants/layout.ts, 600 dp)
    check("the main screen docks the conversation list beside the chat",
      byText(unfolded, CONVERSATION) && byId(unfolded, "agent-chat-scroll"));
    if (!keyboardFrame()) {
      const box = composer(unfolded);
      if (box) tap(box.field);
    }
    await checkBoxOnKeyboard("main");
    screenshot("main-keyboard");

    // turned to landscape on the main screen
    shell("settings", "put", "system", "user_rotation", "1");
    await sleep(3000);
    screenshot("main-landscape");
    check("turning the unfolded phone keeps the app running", appPid() === pid);
    const turned = composer(dumpUi("main-landscape"));
    check("turning keeps the typed text", turned?.field.text.includes(TYPED), turned?.field.text ?? "no box");
    if (!keyboardFrame() && turned) tap(turned.field);
    await checkBoxOnKeyboard("main landscape");
    shell("settings", "put", "system", "user_rotation", "0");
    await sleep(2000);

    // ---- fold again: back to the cover screen ----
    setScreen(COVER);
    await sleep(3000);
    screenshot("cover-folded-again");
    check("folding again keeps the app running", appPid() === pid);
    const refolded = dumpUi("cover-folded-again");
    check("folding again keeps the conversation and the typed text",
      byId(refolded, "agent-chat-scroll") && composer(refolded)?.field.text.includes(TYPED));

    // ---- the emulator's own hinge (a foldable profile's posture, not a size override) ----
    shell("wm", "size", "reset");
    shell("wm", "density", "reset");
    await sleep(2000);
    const hinge = [];
    for (const posture of ["unfold", "fold", "unfold"]) {
      let said;
      try {
        said = adb("emu", posture);
      } catch (e) {
        said = String(e.message);
      }
      await sleep(3000);
      hinge.push(`${posture}: ${said.trim().split("\n")[0]} (${tryShell("wm", "size").trim().replace(/\n/g, "; ")})`);
      screenshot(`hinge-${posture}`);
    }
    fs.writeFileSync(path.join(out, "hinge.txt"), hinge.join("\n") + "\n");
    check("the emulator's hinge folds and unfolds without restarting the app", appPid() === pid, hinge.join(" | "));
  } finally {
    await standIn.close();
  }
}

try {
  await main();
} catch (e) {
  check("the test ran to its end", false, e instanceof Error ? e.message : String(e));
  try {
    screenshot("error");
  } catch {}
}
fs.writeFileSync(path.join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
try {
  fs.writeFileSync(path.join(out, "logcat.txt"), adb("logcat", "-d"));
} catch {}
const failed = results.filter((r) => !r.ok);
console.log(`${results.length - failed.length} of ${results.length} checks passed`);
process.exit(failed.length || results.length === 0 ? 1 : 0);
