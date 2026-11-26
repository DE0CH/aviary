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
// fold and an unfold keep the app (same process), the conversation and the typed text, the main screen docks the
// sidebar, the app takes the landscape shape there; and once on the emulator's real hinge (`adb emu fold` /
// `unfold`). OUT gets a screenshot per step, a screen recording of the keyboard coming, each step's UI dump and
// results.json. Exit 0: every check passed; 1: some failed (a GitHub annotation per failure); 2: setup.
//
// Deterministic (Deyao, 2026-11-25: CI relies on no timing): every step waits for what it changed to show (the UI
// dump, the window manager), never for a length of time; a limit on a wait only stops a hang. What depends on how
// fast the runner is stays out: a flick's speed (the emulator delivers an injected swipe in uneven bursts; the
// decision is unit-tested with exact timestamps in agent-stream/scroll-keyboard-dismiss, its hook included) and the
// system's own rotation (see the landscape step).
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
// the main screen turned: its size the other way round
const LANDSCAPE = { name: "landscape", size: "2184x1968", density: 420 };
const CONVERSATION = "季度銷售報告"; // the stand-in's first conversation (scripts/touch-targets/content.mjs)
const OTHER_CONVERSATION = "活動海報文案"; // its second: a row of the list, never this conversation's own header
const TYPED = "hello fold";
const PLAIN_CONVERSATION = "新產品命名諗法"; // not pinned, nothing running
const PINNED_CONVERSATION = "員工旅行行程"; // pinned: a row that can be dragged
const TASKS_CONVERSATION = "客戶電郵草稿"; // has tasks: its header shows the Activity chip

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
          pkg: attr("package"),
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

/**
 * Tap what `findTarget` finds until `findResult` sees the tap's effect, up to `tries` times. A system dialog that comes
 * up between the look and the tap (the starved emulator's "Pixel Launcher isn't responding") takes the tap; the next
 * look answers it (waitFor) and the tap is made again.
 */
async function tapUntil(findTarget, findResult, label, { tries = 3, waitMs = 60_000 } = {}) {
  for (let attempt = 1; attempt <= tries; attempt += 1) {
    // the earlier tap may have landed late: its effect, else the thing to tap
    const seen = await waitFor((n) => (findResult(n) ? { done: findResult(n) } : findTarget(n) ? { target: findTarget(n) } : null),
      60_000, attempt === tries ? `${label}-target` : undefined);
    if (!seen) return null;
    if (seen.done) return seen.done;
    const target = seen.target;
    tap(target);
    const result = await waitFor(findResult, waitMs, attempt === tries ? label : undefined);
    if (result) return result;
  }
  return null;
}

/** The message box (its root view) and the text field inside it. */
function composer(nodes) {
  const root = byId(nodes, "message-input-root");
  if (!root) return null;
  const field = nodes.find((n) => n.cls === "android.widget.EditText" && within(n, root));
  return field ? { root, field } : null;
}

/** The area of one kind of the window manager's insets on screen (`ime`, `statusBars`; null when it is hidden). */
function insetFrame(type) {
  const dump = tryShell("dumpsys", "window", "displays");
  fs.writeFileSync(path.join(out, "last-insets.txt"), dump);
  for (const line of dump.split("\n")) {
    if (!new RegExp(`type=${type}\\b`).test(line)) continue;
    const frame = line.match(/frame=\[(\d+),(\d+)\]\[(\d+),(\d+)\]/);
    const visible = line.match(/visible=(true|false)/);
    if (frame && visible?.[1] === "true" && +frame[4] > +frame[2]) return { top: +frame[2], bottom: +frame[4] };
  }
  return null;
}

/** The keyboard's area on screen (null when it is hidden). */
const keyboardFrame = () => insetFrame("ime");

/** The screen's pixels now: the colour at (x, y) as [r, g, b]. */
function screenPixels() {
  const raw = execFileSync("adb", ["exec-out", "screencap"], { maxBuffer: 64 * 1024 * 1024, timeout: 60_000 });
  // a profile with two screens prints a warning first; the header is width, height, format (and a colour space)
  for (const header of [16, 12]) {
    for (let skip = 0; skip < 512 && skip + header < raw.length; skip += 1) {
      const w = raw.readUInt32LE(skip);
      const h = raw.readUInt32LE(skip + 4);
      if (w > 0 && h > 0 && w < 10000 && h < 10000 && raw.length - skip - header === w * h * 4) {
        const at = (x, y) => {
          const i = skip + header + (y * w + x) * 4;
          return [raw[i], raw[i + 1], raw[i + 2]];
        };
        return { w, h, at };
      }
    }
  }
  return null;
}
const colourDistance = (a, b) => Math.max(Math.abs(a[0] - b[0]), Math.abs(a[1] - b[1]), Math.abs(a[2] - b[2]));

/** A finger held on (x, y) for `ms`, then lifted, with `during` run while it is down. */
async function hold([x, y], ms, during) {
  shell("input", "motionevent", "DOWN", String(x), String(y));
  try {
    await sleep(ms);
    return during ? await during() : undefined;
  } finally {
    shell("input", "motionevent", "UP", String(x), String(y));
  }
}

/** The list row (its `sidebar-workspace-row-…` view) that holds a title's text node. */
const rowOf = (nodes, text) =>
  nodes
    .filter((n) => n.id.includes("sidebar-workspace-row-") && within(text, n))
    .sort((a, b) => (a.box.bottom - a.box.top) - (b.box.bottom - b.box.top))[0];

const contextMenuOpen = (n) => n.find((m) => m.id.includes("sidebar-workspace-context-menu-"));

/** The Activity panel's header (its title) sits below the status bar on this screen. */
async function checkPanelBelowStatusBar(posture) {
  const title = await waitFor((n) => byId(n, "activity-title"), 60_000, `${posture}-activity`);
  screenshot(`${posture}-activity`);
  const bar = insetFrame("statusBars");
  if (!check(`${posture}: the Activity panel opens`, title)) return;
  check(`${posture}: the Activity panel's header is below the status bar`, bar && title.box.top >= bar.bottom,
    bar ? `title top ${title.box.top}, status bar bottom ${bar.bottom}` : "no status bar inset");
}

async function waitKeyboard(shown, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const frame = keyboardFrame();
    if (shown ? frame : !frame) return shown ? frame : true;
    await sleep(250);
  }
  return shown ? null : false;
}

/**
 * The message box and the keyboard once both have stopped moving (the box rides the keyboard's own animation): the
 * same places in two looks in a row.
 */
async function settledOnKeyboard(label) {
  let last = "";
  return waitFor((n) => {
    const box = composer(n);
    const frame = keyboardFrame();
    if (!box || !frame) {
      last = "";
      return null;
    }
    const now = JSON.stringify([box.root.box, frame]);
    const settled = now === last;
    last = now;
    return settled ? { box, frame, nodes: n } : null;
  }, 60_000, label);
}

const appPid = () => tryShell("pidof", pkg).trim();

// the app's layout once a change of screen has reached it: the wide layout on the main screen (wider than tall when
// turned); on the cover, the message box and no wide layout
const wideLayout = (n) => byId(n, "wide-shell");
const wideShape = (wide) => (n) => {
  const b = byId(n, "wide-shell")?.box;
  return b && (b.right - b.left > b.bottom - b.top) === wide ? b : null;
};
const coverLayout = (n) => (n.length > 0 && !byId(n, "wide-shell") && byId(n, "message-input-root") ? n : null);

function setScreen({ size, density }) {
  shell("wm", "size", size);
  shell("wm", "density", String(density));
}

/** The message box sits on the keyboard: not covered by it, and not floating far above it. */
async function checkBoxOnKeyboard(posture) {
  if (!check(`${posture}: the keyboard opens from the message box`, await waitKeyboard(true))) return;
  const settled = await settledOnKeyboard(`${posture}-keyboard`);
  const box = settled?.box;
  const frame = settled?.frame;
  if (!check(`${posture}: the message box is on screen with the keyboard up`, box)) return;
  const gap = frame.top - box.root.box.bottom;
  // above the keyboard (a pixel of rounding), and within the box's own margin and the toolbar's height of it
  check(`${posture}: the message box rides on the keyboard`, gap >= -2 && gap <= 160,
    `box bottom ${box.root.box.bottom}, keyboard top ${frame.top}, gap ${gap}px`);
}

// ---- the path ---------------------------------------------------------------------------------------------------

async function main() {
  const standIn = await startStandIn({
    appDir: path.join(path.dirname(new URL(import.meta.url).pathname), "../../packages/app/public"),
    port: PORT,
    appScheme: pkg,
    quiet: true,
  });
  try {
    adb("reverse", `tcp:${PORT}`, `tcp:${PORT}`);
    // Chrome opens the sign-in (a Custom Tab): no first-run screens in the way
    tryShell("am", "set-debug-app", "--persistent", "com.android.chrome");
    tryShell("sh", "-c", "echo '_ --disable-fre --no-default-browser-check --no-first-run' > /data/local/tmp/chrome-command-line");
    shell("logcat", "-G", "32M"); // room for the whole run's log
    // a fresh emulator's Play Store updates itself and starves everything else (the launcher stops responding)
    tryShell("pm", "disable-user", "--user", "0", "com.android.vending");
    // and the system's own "isn't responding" and crash dialogs are never shown: one over the screen hid the app for the
    // whole run (main's c5a92b3d82, "Pixel Launcher isn't responding"), and a wait for the emulator to settle is a time
    shell("settings", "put", "global", "hide_error_dialogs", "1");
    // the emulator has a hardware keyboard: show the on-screen one anyway, as on a phone
    shell("settings", "put", "secure", "show_ime_with_hard_keyboard", "1");
    shell("settings", "put", "system", "accelerometer_rotation", "0");
    shell("settings", "put", "system", "user_rotation", "0");
    adb("install", "-r", "-g", apk);

    // ---- folded: the cover screen ----
    setScreen(COVER);
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

    // the drawer opens from its button, then the conversation from its row: each tap made again if a dialog took it.
    // Open means the chat and its box with the drawer gone (the chat is in the tree under an open drawer too).
    const isOpen = (n) => byId(n, "agent-chat-scroll") && composer(n) && !byId(n, "sidebar-close");
    let opened = null;
    if (await tapUntil((n) => byId(n, "menu-button"), (n) => byText(n, CONVERSATION), "conversation-row")) {
      opened = await tapUntil((n) => byText(n, CONVERSATION), isOpen, "conversation");
    }
    screenshot("cover-conversation");
    if (!check("a conversation opens from the conversation list", opened)) return;

    // the keyboard comes and goes, recorded
    const recording = spawn("adb", ["shell", "screenrecord", "--time-limit", "20", "/sdcard/keyboard.mp4"]);
    // its end, caught from the start: it may well be over before the steps below are
    const recorded = new Promise((done) => {
      recording.on("exit", done);
      recording.on("error", done);
    });
    tap((await waitFor(composer, 60_000, "cover-box")).field);
    await checkBoxOnKeyboard("cover");
    shell("input", "text", TYPED.replace(/ /g, "%s"));
    const typed = await waitFor((n) => {
      const box = composer(n);
      return box?.field.text.includes(TYPED) ? box : null;
    }, 60_000, "cover-typed");
    screenshot("cover-keyboard");
    check("typed text lands in the message box", typed, typed?.field.text ?? composer(dumpUi())?.field.text ?? "no box");

    await Promise.race([recorded, sleep(30_000)]);
    try {
      fs.writeFileSync(path.join(out, "keyboard-cover.mp4"),
        execFileSync("adb", ["exec-out", "cat", "/sdcard/keyboard.mp4"], { maxBuffer: 256 * 1024 * 1024, timeout: 120_000 }));
    } catch {
      console.log("::warning title=android emulator test::no screen recording");
    }

    // ---- unfold: the main screen, with the keyboard up ----
    if (!keyboardFrame()) tap(composer(dumpUi()).field);
    await waitKeyboard(true);
    setScreen(MAIN);
    await waitFor(wideLayout, 60_000, "main-unfolded");
    screenshot("main-unfolded");
    check("unfolding keeps the app running (no restart)", appPid() === pid, `pid ${pid} → ${appPid()}`);
    const unfolded = dumpUi("main-unfolded");
    check("unfolding keeps the conversation and the typed text",
      byId(unfolded, "agent-chat-scroll") && composer(unfolded)?.field.text.includes(TYPED),
      composer(unfolded)?.field.text ?? "no box");
    // 1968 px at density 420 is 750 dp: the wide layout, whose conversation list docks beside the chat instead of
    // covering it (constants/layout.ts, 600 dp). It keeps the phone's closed list: its button opens it.
    check("the main screen has the wide layout", byId(unfolded, "wide-shell"));
    if (!byText(unfolded, OTHER_CONVERSATION)) {
      const toggle = byId(unfolded, "menu-button");
      if (toggle) tap(toggle);
    }
    const docked = await waitFor((n) => {
      const row = byText(n, OTHER_CONVERSATION);
      const chat = byId(n, "agent-chat-scroll");
      return row && chat && chat.box.left >= row.box.right - 2 ? { row, chat } : null;
    }, 60_000, "docked");
    screenshot("main-list-docked");
    check("the main screen docks the conversation list beside the chat", docked,
      docked ? `list row to x=${docked.row.box.right}, chat from x=${docked.chat.box.left}` : "");
    const withList = composer(dumpUi());
    if (withList && !keyboardFrame()) tap(withList.field);
    await checkBoxOnKeyboard("main");
    screenshot("main-keyboard");

    // turned to landscape on the main screen: the screen's size the other way round, the configuration change a turn
    // sends the app (orientation landscape), applied before `wm size` returns. The system's own rotation is not used:
    // on the foldable profile it followed neither `wm user-rotation lock 1` nor the rotation settings (2026-11-26: user
    // rotation 90 left in "free" mode, the screen still 1968 x 2184 at the end of the run).
    setScreen(LANDSCAPE);
    await waitFor(wideShape(true), 60_000, "landscape");
    screenshot("main-landscape");
    check("turning the unfolded phone keeps the app running", appPid() === pid);
    const landscape = dumpUi("main-landscape");
    const shellBox = byId(landscape, "wide-shell")?.box;
    check("the unfolded phone takes the landscape shape", shellBox && shellBox.right - shellBox.left > shellBox.bottom - shellBox.top,
      shellBox ? `${shellBox.right - shellBox.left} x ${shellBox.bottom - shellBox.top}` : "no wide layout");
    const turned = composer(landscape);
    check("turning keeps the typed text", turned?.field.text.includes(TYPED), turned?.field.text ?? "no box");
    if (!keyboardFrame() && turned) tap(turned.field);
    await checkBoxOnKeyboard("main landscape");
    setScreen(MAIN);
    await waitFor(wideShape(false), 60_000, "main-portrait-again");

    // ---- fold again: back to the cover screen ----
    setScreen(COVER);
    await waitFor(coverLayout, 60_000, "cover-folded-again");
    screenshot("cover-folded-again");
    check("folding again keeps the app running", appPid() === pid);
    const refolded = dumpUi("cover-folded-again");
    check("folding again keeps the conversation and the typed text",
      byId(refolded, "agent-chat-scroll") && composer(refolded)?.field.text.includes(TYPED));

    // ---- the list and the side panel on both screens (Deyao, 2026-11-20: no sign of a tap on the unfolded phone,
    // no menu on a long press, and the Activity panel under the status bar) ----
    setScreen(MAIN);
    await waitFor((n) => byId(n, "wide-shell"), 60_000, "main-again");
    if (!byText(dumpUi(), PLAIN_CONVERSATION)) {
      const toggle = byId(dumpUi(), "menu-button");
      if (toggle) tap(toggle);
    }
    const plain = await waitFor((n) => byText(n, PLAIN_CONVERSATION), 60_000, "plain-row");
    if (check("main: the conversation list is docked", plain)) {
      // the row's own ground (its view, left of the title, clear of the letters), then the same point under a held
      // finger: the press shows at once
      const rowBox = rowOf(dumpUi("plain-row"), plain)?.box ?? plain.box;
      const point = [rowBox.left + 3, Math.round((rowBox.top + rowBox.bottom) / 2)];
      const before = screenPixels()?.at(...point);
      // the picture is taken as soon as the finger is down. The finger stays down past the long-press time, so the
      // press always ends as a long press (the lift's event time is later than the long-press timer's, however slow
      // the runner): its menu opens, and is put away
      const [x, y] = centre(plain);
      shell("input", "motionevent", "DOWN", String(x), String(y));
      let during;
      try {
        during = screenPixels()?.at(...point);
        await sleep(900);
      } finally {
        shell("input", "motionevent", "UP", String(x), String(y));
      }
      if (await waitFor(contextMenuOpen, 60_000, "press-menu")) shell("input", "keyevent", "KEYCODE_BACK");
      await waitFor((n) => !contextMenuOpen(n) && byText(n, PLAIN_CONVERSATION), 60_000, "press-menu-closed");
      check("main: a finger on a conversation shows at once", before && during && colourDistance(before, during) >= 8,
        `row ${before} under the finger ${during}`);

      // a long press opens the conversation's menu at the row
      const row = await waitFor((n) => byText(n, PLAIN_CONVERSATION), 60_000, "plain-row-2");
      const menu = row ? (await hold(centre(row), 900), await waitFor(contextMenuOpen, 60_000, "row-menu")) : null;
      screenshot("main-row-menu");
      check("main: a long press on a conversation opens its menu", menu);
      check("main: the menu opens at the row", menu && row && menu.box.top <= row.box.bottom + 40 && menu.box.bottom >= row.box.top - 40,
        menu && row ? `row ${row.box.top}-${row.box.bottom}, menu ${menu.box.top}-${menu.box.bottom}` : "");
      shell("input", "keyevent", "KEYCODE_BACK");
      await waitFor((n) => !contextMenuOpen(n) && byText(n, PINNED_CONVERSATION), 60_000, "row-menu-closed");

      // a pinned conversation, which can be dragged: holding still opens its menu too
      const pinned = await waitFor((n) => byText(n, PINNED_CONVERSATION), 60_000, "pinned-row");
      const pinnedMenu = pinned ? (await hold(centre(pinned), 900), await waitFor(contextMenuOpen, 60_000, "pinned-menu")) : null;
      screenshot("main-pinned-menu");
      check("main: a long press on a pinned conversation opens its menu", pinnedMenu);
      shell("input", "keyevent", "KEYCODE_BACK");
      await waitFor((n) => !contextMenuOpen(n) && byText(n, TASKS_CONVERSATION), 60_000, "pinned-menu-closed");
    }

    // the Activity panel, docked beside the chat on the main screen and over it on the cover
    const busy = await waitFor((n) => byText(n, TASKS_CONVERSATION), 60_000, "tasks-row");
    if (busy) tap(busy);
    const chip = await waitFor((n) => byId(n, "activity-chip"), 60_000, "activity-chip");
    if (check("a conversation that ran tasks shows the Activity chip", chip)) {
      tap(chip);
      await checkPanelBelowStatusBar("main");
      setScreen(COVER);
      // the cover's layout first, so the panel looked at is the cover's
      await waitFor((n) => (n.length > 0 && !byId(n, "wide-shell") ? n : null), 60_000, "cover-activity-layout");
      await checkPanelBelowStatusBar("cover");
      // Android's Back closes the panel (yyc/side-panel-back.ts): it had left the app for the launcher (2026-11-26)
      shell("input", "keyevent", "KEYCODE_BACK");
      const closed = await waitFor(
        (n) => (n.some((m) => m.pkg === pkg) && !byId(n, "activity-title") && byId(n, "message-input-root") ? n : null)
          || (n.length > 0 && !n.some((m) => m.pkg === pkg) ? "left" : null),
        60_000, "activity-closed");
      check("cover: Android's Back closes the Activity panel and stays in the app", closed && closed !== "left",
        closed === "left" ? "the app left the screen" : "");
      // the hinge below folds the app, not the launcher (an app left by Back keeps its process, all the old check saw)
      if (closed === "left") {
        shell("am", "start", "-n", `${pkg}/.MainActivity`);
        await waitFor(coverLayout, 60_000, "app-again");
      }
    }

    // ---- the emulator's own hinge (a foldable profile's posture, not a size override) ----
    shell("wm", "size", "reset");
    shell("wm", "density", "reset");
    const hinge = [];
    for (const posture of ["unfold", "fold", "unfold"]) {
      let said;
      try {
        said = adb("emu", posture);
      } catch (e) {
        said = String(e.message);
      }
      // the posture's layout in the app: the wide one unfolded, the cover's folded
      const reached = await waitFor(posture === "unfold" ? wideLayout : coverLayout, 60_000, `hinge-${posture}`);
      check(`the emulator's hinge: the app takes the ${posture === "unfold" ? "main screen's" : "cover's"} layout (${posture})`, reached);
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
