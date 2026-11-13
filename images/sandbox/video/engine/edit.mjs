#!/usr/bin/env node
// edit: render a video from a JSON edit plan. ffmpeg cuts and conforms each source clip exactly and mixes the
// music and sound effects; the plan becomes one HyperFrames page (index.html + this folder's src/, which draws the
// transitions, captions, texts and graphics at any moment), and HyperFrames renders that page to video.
//
//   edit example                       a documented example plan: every core field
//   edit effects                       the motion graphics: camera moves, text styles, graphics, transitions, sound
//   edit assets [music|sfx|emoji|fonts]   the library of music, sound effects, animated emoji and fonts
//   edit beats FILE [--from S --to S]  a music track's tempo and beat times
//   edit check PLAN.json               validate, print the timeline (where each clip lands in the output)
//   edit frame PLAN.json --at S [--out FILE.png]   one still of the finished timeline at output second S
//   edit sheet PLAN.json [--from S --to S --every S --out FILE.jpg]   several stills in one picture, to check motion
//   edit render PLAN.json              render to the plan's "output" (H.264 + AAC, loudness-normalised)
//
// Paths in a plan are relative to the folder `edit` is run from. Nothing here generates footage: it cuts, places,
// moves and draws text and graphics over the footage it is given.
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CACHE = process.env.EDIT_CACHE || path.join(os.homedir(), ".cache", "edit");
const die = (m) => { console.error("edit: " + m); process.exit(1); };

const ASSETS = process.env.EDIT_ASSETS || "/opt/assets";

const EXAMPLE = {
  output: "exports/example.mp4",
  width: 1080, height: 1920, fps: 30,
  theme: "bold", sfx: "auto",
  captionStyle: { look: "outline", position: "bottom", animate: "pop" },
  clips: [
    { card: "標題卡", subtitle: "副標題（可省略）", seconds: 2, emoji: "🔥", transition: "flash" },
    { src: "videos/A.MP4", start: 2.0, end: 8.5, fit: "cover", transition: "whip-left", motion: "zoom-in",
      punches: [{ at: 5.2, scale: 1.3, seconds: 0.8 }],
      captions: [{ start: 2.1, end: 4.4, text: "字幕用原片的秒數，*重點*會變色" }],
      texts: [{ text: "左上角標籤", start: 0, end: 3, position: "top-left", style: "tag" }],
      graphics: [{ type: "emoji", emoji: "😂", start: 3, end: 5, x: 80, y: 30 }] },
    { src: "videos/B.MP4", start: 10, end: 14, speed: 1.5, volume: 0.6, fit: "blur", look: "vivid" },
    { image: "uploads/logo.png", seconds: 2, fit: "contain" },
  ],
  texts: [{ text: "全片標題", start: 0.5, end: 3, position: "top", style: "impact" }],
  graphics: [{ type: "confetti", start: 12 }],
  music: { src: "assets/music/upbeat/upbeat-jazz-644.mp3", volume: 0.15 },
};
const HELP = `A plan is one JSON object:
  output        where the video goes, e.g. "exports/name.mp4"
  width,height  default 1080 x 1920;  fps default 30
  theme         the overall look: "clean" (default) | "bold" | "neon" | "cute" | "news" | "comic"; sets fonts, accent
                colours, caption look, card backgrounds. "accent", "accent2" (colours) and "font" override parts of it.
  clips         the timeline, in order. Each item is ONE of:
    {"src": FILE, "start": S, "end": S}   a stretch of a video, in the SOURCE file's seconds
        fit      "cover" (fill the frame, crop the overflow; default), "contain" (whole picture, black bars),
                 "blur" (whole picture over a blurred copy of itself)
        speed    0.25-4 (default 1; sound keeps its pitch)      volume  0-2 (default 1; 0 = silent)
        captions [{"start": S, "end": S, "text": "…"}] in the SOURCE file's seconds (as speech-times reports them);
                 they follow the clip wherever it lands and through speed changes. *stars* mark an emphasised word.
        texts, graphics, sounds   as at the top level, but seconds count from this clip's own beginning; add
                 "time": "source" to one of them to give its start/end in the source file's seconds instead
        motion, punches, look, inset   camera moves and looks: see \`edit effects\`
    {"image": FILE, "seconds": N}         a still picture (fit, motion, look as above)
    {"card": "title", "subtitle": "…", "seconds": N}   an animated text card; optional "emoji", "background" (colour),
                 "backdrop": "plain" | "rings" | "gradient" | "dots" | "grid" | "rays", "color"
    any item may carry "transition" (how it changes INTO THE NEXT item) and "transitionSeconds" (default 0.4):
        ${"TRANS"}
        The two items overlap for that long, so the video gets shorter by it. No transition = a hard cut.
  texts         [{"text","start","end", …}] over the whole video, in OUTPUT seconds: see \`edit effects\` for styles
  graphics      [{"type", "start", …}] emoji, confetti, arrows, circles, spotlights …: see \`edit effects\`
  captionStyle  {"look": "outline" | "box" | "pill" | "karaoke" | "reveal", "position": "bottom" | "center" | "top",
                 "animate": "none" | "pop" | "rise" | "slam", "size": px, "color", "accent", "font", "maxWidth": percent, "y": percent}
  music         {"src": FILE, "volume": 0-1 under speech (default 0.18), "volumeAlone": over cards and silent clips
                 (default 4x), "start": seconds into the track, "fadeOut": seconds (default 1.5)}; loops
  sfx           "auto": a fitting sound effect on every transition, popping text and graphic.  sounds: explicit ones
  progressBar   true: a thin bar along the bottom edge showing how far the video is
  html          [{"file": FILE.html, "start": S, "end": S}] your own HyperFrames block drawn above the footage for
                 that stretch: see \`edit effects\`, "YOUR OWN HTML"
  normalize     false to skip the final loudness normalisation (default true)
"\\n" inside a text starts a new line.  Library of music, sound effects, emoji and fonts: \`edit assets\`.`;

const EFFECTS_EXAMPLE = {
  output: "exports/promo.mp4", theme: "bold", sfx: "auto", progressBar: true,
  clips: [
    { card: "全場*最型*\n機械人", subtitle: "小蓋登場", emoji: "🤖", seconds: 1.8, transition: "flash" },
    { src: "videos/A.MP4", start: 7.7, end: 13.2, motion: "zoom-in", transition: "whip-left",
      punches: [{ at: 9.1, scale: 1.35, seconds: 0.9, focus: [50, 30] }],
      captions: [{ start: 7.8, end: 10.5, text: "我係全場*最型*嘅機械人" }],
      texts: [{ text: "最型!", start: 9.1, end: 10.4, time: "source", style: "stamp", x: 72, y: 24 },
              { text: "小蓋", sub: "機械人店員", start: 0.3, end: 3.5, style: "lower-third" }],
      graphics: [{ type: "emoji", emoji: "😎", start: 9.1, end: 10.6, time: "source", x: 22, y: 26, loop: "wiggle" },
                 { type: "speedlines", start: 9.1, end: 9.9, time: "source", y: 30 }] },
    { src: "videos/B.MP4", start: 12.3, end: 15.3, inset: 0.78, motion: "pulse", transition: "zoom",
      texts: [{ text: "識跳舞", start: 0, end: 3, style: "burst", y: 9 }],
      graphics: [{ type: "emoji-rain", emoji: "🎶", start: 0.2, end: 3 }, { type: "sparkles", start: 0, end: 3, y: 50, w: 90, h: 60 }] },
    { card: "快啲嚟*探我*啦！", seconds: 2.2, emoji: "👋" },
  ],
  graphics: [{ type: "confetti", start: 10.2 }],
  music: { src: "assets/music/energetic/NAME.mp3", volume: 0.14 },
};
const EFFECTS = `Motion graphics the plan can ask for. Every time is in seconds; x, y, w, h are percent of the frame
(x 0 = left edge, y 0 = top); sizes are percent of the frame's width.

CAMERA (on a clip or image)
  motion   "zoom-in" | "zoom-out" | "pan-left" | "pan-right" | "pan-up" | "pan-down" | "shake" | "pulse" | "handheld"
           or {"type": …, "amount": 1.15 (how far it zooms), "focus": [x, y] (what it zooms towards)}
  punches  [{"at": S (source seconds), "scale": 1.3, "seconds": 0.8, "focus": [x, y]}]  a sudden punch-in on a word
  look     "vivid" | "warm" | "cool" | "bw" | "vintage" | "dreamy" | "punchy" | "dark"
  inset    0.5-0.95: the clip shown smaller as a rounded card over the theme's animated background ("backdrop",
           "background" choose it; "insetY" moves it up (-) or down (+) in percent)

TRANSITIONS  ${"TRANS"}

TEXTS  {"text", "start", "end", "style", "position" or "x","y", "size", "color", "accent", "font", "rotate",
        "animate", "out", "loop"}.  *stars* inside a text colour that word with the accent.
  style    title | plain | label | tag (pill) | impact (huge, tilted, outlined) | neon (glow) | gradient | stamp
           (slams in, bordered, tilted) | bubble (speech bubble; "tail": "left" | "right" | "none") | burst (comic
           star behind the words) | highlight (marker pen drawn behind) | lower-third (name bar; "sub" is its second
           line) | counter (a number counting up: "count": [0, 100], "prefix", "suffix"; "text" is its label)
  animate  fade | pop | slide-left | slide-right | slide-up | drop | stamp | spin | chars (letters one by one) |
           typewriter | none.   out: fade | pop | slide-left | slide-right | slide-up | drop
  loop     wiggle | pulse | float | shake   (keeps moving while it is on screen)
  position top | upper | center | lower | bottom | left | right | top-left | top-right | bottom-left | bottom-right
  font     hei (default) | round | cute | serif | kai (brush) | display (poster) | pixel | impact | poppins | bungee

GRAPHICS  {"type", "start", "end" (or "seconds"), …}
  emoji        {"emoji": "🔥", "x", "y", "size": 22, "animate": pop | drop | spin | slide-left | slide-right | fade,
                "loop": wiggle | pulse | float | shake | spin | bounce}   animated when the emoji is in \`edit assets emoji\`
  emoji-rain   {"emoji": "❤️", "count": 18, "direction": "down" | "up", "size": 11}
  confetti     falls over the whole frame; with "x","y" it bursts from that point. "colors": ["#…"]
  arrow        points at ("x","y"); "angle" is where it comes from (0 right, 90 below, 180 left, 270 above), "size", "color"
  circle       a hand-drawn ring around ("x","y"), "w","h" wide and high, "color"
  spotlight    darkens everything except a circle at ("x","y") of radius "r"
  speedlines   comic speed lines converging on ("x","y")
  sparkles     twinkling stars in the box ("x","y","w","h")
  flash        a white (or "color") flash          light-leak   a warm film light leak ("seed" 1-9 picks the shape)
  bars         cinema letterbox bars ("size")      vignette     dark corners        tint   a colour wash ("color","opacity")
  image        {"image": FILE, "x","y","size"} a logo or sticker (PNG with transparency), same animate/loop as emoji

YOUR OWN HTML  (for a look none of the above gives)
  "html": [{"file": "work/badge.html", "start": 3, "end": 6}]   the file is an HTML fragment (elements, a <style>,
  a <script>) laid over the whole frame for that stretch, above the footage and below texts and captions. It follows
  HyperFrames' rules: animate with a paused GSAP timeline (window.gsap is loaded) registered as
  window.__timelines["NAME"], or with CSS animations; times inside it count from the video's start, in seconds;
  no setTimeout, no Date, no random without a seed. Position with absolute pixels in the frame (1080 x 1920 unless the plan says otherwise).

SOUND
  sfx: "auto" at the top level adds whooshes, pops and hits to transitions, texts and graphics by itself.
  Any text, graphic or clip transition can carry "sfx": NAME to choose its sound, or "sfx": false for none.
  sounds   [{"sfx": NAME or "src": FILE, "at": S, "volume": 0-1 (default 0.6)}]
  NAME     a shortcut (${"SFXNAMES"}) or a file from \`edit assets sfx\`, e.g. "whoosh/arrow-whoosh-1491"
  music    files in \`edit assets music\` with their mood and beats per minute; \`edit beats FILE\` lists a track's
           beat times so cuts and punches can land on the beat ("start" in music shifts the track).`;

const TRANS = ["fade", "slide-left", "slide-right", "slide-up", "slide-down", "wipe", "flash", "zoom", "whip-left", "whip-right", "whip-up", "whip-down", "glitch", "spin", "flip", "clock-wipe", "iris"];
const FITS = ["cover", "contain", "blur"];
const THEMES = ["clean", "bold", "neon", "cute", "news", "comic"];
const MOTIONS = ["zoom-in", "zoom-out", "pan-left", "pan-right", "pan-up", "pan-down", "shake", "pulse", "handheld"];
const LOOKS = ["vivid", "warm", "cool", "bw", "vintage", "dreamy", "punchy", "dark"];
const BACKDROPS = ["plain", "rings", "gradient", "dots", "grid", "rays"];
const TEXT_STYLES = ["title", "plain", "label", "tag", "impact", "neon", "gradient", "stamp", "bubble", "burst", "highlight", "lower-third", "counter"];
const TEXT_ANIMS = ["fade", "pop", "slide-left", "slide-right", "slide-up", "drop", "stamp", "spin", "chars", "typewriter", "none"];
const TEXT_DEFAULT_ANIM = { tag: "pop", bubble: "pop", burst: "pop", impact: "pop", stamp: "stamp", "lower-third": "slide-right", neon: "chars", gradient: "chars", counter: "pop" };
const LOOPS = ["wiggle", "pulse", "float", "shake", "spin", "bounce"];
const GRAPHICS = { emoji: 2, "emoji-rain": 3, confetti: 3, arrow: 2, circle: 2, spotlight: 2, speedlines: 0.8, sparkles: 2, flash: 0.35, "light-leak": 1.5, bars: null, vignette: null, tint: null, image: 2, lottie: 2 };
const CAP_LOOKS = ["outline", "box", "pill", "karaoke", "reveal"], CAP_ANIMS = ["none", "pop", "rise", "slam"];
const FONTS = ["hei", "round", "cute", "serif", "kai", "display", "pixel", "impact", "poppins", "bungee"];
// sound-effect shortcuts -> a file under assets/sfx (without .mp3)
const SFX = {
  whoosh: "swoosh/fast-transitions-swoosh-3115", swoosh: "swoosh/short-wind-swoosh-1461", sweep: "transition/fast-small-sweep-transition-166",
  zoom: "transition/air-zoom-vacuum-2608", pop: "pop/bubble-pop-up-alert-notification-2357", click: "click/modern-technology-select-3124",
  ding: "ding/relaxing-bell-chime-3109", correct: "notification/correct-answer-tone-2870", notify: "notification/confirmation-tone-2867",
  impact: "drum/short-bass-hit-2299", hit: "impact/wood-hard-hit-2182", boom: "impact/cinematic-whoosh-deep-impact-1143",
  riser: "riser/cinematic-trailer-riser-790", glitch: "glitch/small-electric-glitch-2595", win: "win/quick-win-video-game-notification-269",
  boing: "cartoon/boing-hit-sound-2894", fail: "cartoon/trombone-disappoint-744", camera: "camera/camera-shutter-hard-click-1430",
  magic: "magic/fairy-magic-sparkle-871", applause: "applause/small-group-cheer-and-applause-518", laugh: "laugh/crowd-laugh-424",
  typing: "typing/single-key-type-2533", robot: "robot/electric-pop-2365", whistle: "cartoon/cartoon-toy-whistle-616", kiss: "cartoon/cartoon-friendly-kiss-2194",
};
const AUTO_TRANS_SFX = { flash: "camera", zoom: "zoom", glitch: "glitch", spin: "swoosh", flip: "swoosh", "clock-wipe": "sweep", iris: "sweep", wipe: "sweep" };
const AUTO_TEXT_SFX = { pop: "pop", stamp: "hit", drop: "hit", spin: "swoosh" };
const AUTO_GFX_SFX = { emoji: "pop", confetti: "win", arrow: "swoosh", "emoji-rain": "magic", sparkles: "magic", flash: "camera", speedlines: "whoosh", spotlight: "sweep", circle: "click" };
const fill = (s) => s.replace("TRANS", TRANS.join(" | ")).replace("SFXNAMES", Object.keys(SFX).join(", "));
const readJson = (f, d) => { try { return JSON.parse(fs.readFileSync(f, "utf8")); } catch { return d; } };

const [cmd, planArg, ...rest] = process.argv.slice(2);
if (!cmd || cmd === "help" || cmd === "--help") { console.log(fs.readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 16).map((l) => l.replace(/^\/\/ ?/, "")).join("\n")); process.exit(0); }
if (cmd === "example") { console.log(JSON.stringify(EXAMPLE, null, 2) + "\n\n" + fill(HELP)); process.exit(0); }
if (cmd === "effects") { console.log(fill(EFFECTS) + "\n\nAn example that uses them:\n" + JSON.stringify(EFFECTS_EXAMPLE, null, 1)); process.exit(0); }
const opt = (name) => { const all = [planArg, ...rest]; const i = all.indexOf(name); return i >= 0 ? all[i + 1] : undefined; };
if (cmd === "assets") {
  const kind = planArg;
  const music = readJson(path.join(ASSETS, "music/index.json"), []), sfx = readJson(path.join(ASSETS, "sfx/index.json"), []), emoji = readJson(path.join(ASSETS, "emoji/index.json"), []);
  if (!kind) console.log(`The library is in assets/ (use these paths in a plan as they are):\n  edit assets music   ${music.length} tracks by mood, with length and beats per minute\n  edit assets sfx     ${sfx.length} sound effects by kind, and the shortcut names\n  edit assets emoji   ${emoji.length} animated emoji (any other emoji still works, as a still picture)\n  edit assets fonts   the font names a plan can use\nFree to use in videos, commercial ones included, with no credit needed (Mixkit licence; emoji: Google Noto, CC BY 4.0).`);
  if (kind === "music") { let cat = ""; for (const m of music) { if (m.category !== cat) console.log(`\n${cat = m.category}`); console.log(`  assets/${m.file}   ${m.seconds} s, ${m.bpm || "?"} bpm   "${m.title}"`); } console.log("\nBeat times of a track: edit beats FILE"); }
  if (kind === "sfx") { console.log("shortcuts (use as \"sfx\": NAME):"); for (const [k, v] of Object.entries(SFX)) console.log(`  ${k.padEnd(9)} = ${v}`); let cat = ""; for (const m of sfx) { if (m.category !== cat) console.log(`\n${cat = m.category}`); console.log(`  ${m.file.replace(/^sfx\//, "").replace(/\.mp3$/, "")}   ${m.seconds} s`); } }
  if (kind === "emoji") console.log("animated: " + emoji.map((e) => `${e.emoji} ${e.name}`).join("   ") + "\nGive the emoji itself or its name. Any other emoji is drawn as a still.");
  if (kind === "fonts") console.log(`hei      clean sans, the default (Chiron Hei HK)\nround    rounded, friendly (Chiron GoRound TC)\ncute     soft round (Huninn)\nserif    print serif (Chiron Sung HK)\nkai      brush handwriting (LXGW WenKai TC)\ndisplay  very heavy poster face (Dela Gothic One; Cantonese-only characters such as 嘅 fall back to hei)\npixel    retro game (DotGothic16; same fallback)\nimpact   tall condensed Latin and digits (Anton)\npoppins  geometric Latin and digits\nbungee   chunky Latin sign lettering\nAll have full Cantonese coverage except where noted.`);
  process.exit(0);
}
if (cmd === "beats") {
  if (!planArg) die("usage: edit beats FILE [--from S] [--to S]");
  const f = fs.existsSync(planArg) ? planArg : path.join(ASSETS, planArg.replace(/^assets\//, ""));
  if (!fs.existsSync(f)) die(`no such file ${planArg}`);
  const beats = execFileSync("aubiotrack", [f]).toString().split(/\s+/).filter(Boolean).map(Number);
  const gaps = beats.slice(1).map((b, i) => b - beats[i]).sort((a, b) => a - b);
  const a = parseFloat(opt("--from") ?? "0"), b = parseFloat(opt("--to") ?? "30");
  console.log(`${planArg}: ${gaps.length ? Math.round(60 / gaps[gaps.length >> 1]) : "?"} beats per minute; beats between ${a} and ${b} s of the track:`);
  console.log(beats.filter((x) => x >= a && x <= b).map((x) => x.toFixed(2)).join("  "));
  process.exit(0);
}
if (!["check", "frame", "sheet", "render"].includes(cmd)) die(`unknown command "${cmd}" (example | effects | assets | beats | check | frame | sheet | render)`);
if (!planArg) die(`usage: edit ${cmd} PLAN.json`);

// ---- read and validate the plan
let plan;
try { plan = JSON.parse(fs.readFileSync(planArg, "utf8")); } catch (e) { die(`cannot read ${planArg} as JSON: ${e.message}`); }
const W = plan.width ?? 1080, H = plan.height ?? 1920, FPS = plan.fps ?? 30;
const errs = [];
const num = (v) => typeof v === "number" && isFinite(v);
const oneOf = (v, list, where, what) => { if (v != null && !list.includes(v)) errs.push(`${where}: ${what} "${v}" is not one of ${list.join(", ")}`); };
const asset = (p) => (typeof p !== "string" ? null : fs.existsSync(p) ? p : fs.existsSync(path.join(ASSETS, p.replace(/^assets\//, ""))) ? path.join(ASSETS, p.replace(/^assets\//, "")) : null);
if (!Number.isInteger(W) || !Number.isInteger(H) || W % 2 || H % 2 || W < 64 || H < 64) errs.push(`width and height must be even whole numbers (got ${W} x ${H})`);
if (!num(FPS) || FPS < 1 || FPS > 60) errs.push(`fps must be 1-60 (got ${FPS})`);
if (!Array.isArray(plan.clips) || !plan.clips.length) errs.push(`"clips" must be a list with at least one item`);
if (cmd === "render" && (typeof plan.output !== "string" || !/\.mp4$/i.test(plan.output))) errs.push(`"output" must be a path ending in .mp4`);
const legacyFlashy = plan.motion === "flashy";           // an earlier plan field: the neon look with flashes on the cuts
if (legacyFlashy) { plan.theme ??= "neon"; plan.progressBar ??= true; }
oneOf(plan.theme, THEMES, "the plan", "theme");
oneOf(plan.captionStyle?.look, CAP_LOOKS, "captionStyle", "look"); oneOf(plan.captionStyle?.animate, CAP_ANIMS, "captionStyle", "animate");
const probe = (file, ...args) => execFileSync("ffprobe", ["-v", "error", ...args, file]).toString().trim();
const durations = new Map();
const srcDuration = (f) => { if (!durations.has(f)) durations.set(f, parseFloat(probe(f, "-show_entries", "format=duration", "-of", "csv=p=0"))); return durations.get(f); };

const emojiIndex = readJson(path.join(ASSETS, "emoji/index.json"), []);
const bare = (s) => String(s).replace(/️/g, "");
const lotties = {};
const lottieFor = (e) => {                      // an emoji (or its name) -> the key of its animation, or null
  const hit = emojiIndex.find((x) => x.name === e || bare(x.emoji) === bare(e));
  if (!hit) return null;
  lotties[hit.name] ??= readJson(path.join(ASSETS, hit.file), null);
  return lotties[hit.name] ? hit.name : null;
};
const sfxFile = (name, where) => {
  if (name == null || name === false) return null;
  const tries = [name, SFX[name] && path.join(ASSETS, "sfx", SFX[name] + ".mp3"), path.join(ASSETS, "sfx", String(name).replace(/^(assets\/)?(sfx\/)?/, "") + (/\.\w+$/.test(name) ? "" : ".mp3"))];
  const f = tries.find((t) => t && fs.existsSync(t) && fs.statSync(t).isFile());
  if (!f) errs.push(`${where}: no sound effect "${name}" (shortcuts: ${Object.keys(SFX).join(", ")}; all files: edit assets sfx)`);
  return f || null;
};

const checkTimed = (list, where, max, kind) => (Array.isArray(list) ? list : list == null ? [] : (errs.push(`${where} ${kind}: must be a list`), [])).forEach((t, j) => {
  const at = `${where} ${kind}[${j}]`;
  if (kind === "texts") {
    oneOf(t.style, TEXT_STYLES, at, "style"); oneOf(t.animate, TEXT_ANIMS, at, "animate"); oneOf(t.loop, LOOPS, at, "loop"); oneOf(t.font, FONTS, at, "font");
    if (t.style !== "counter" && (typeof t.text !== "string" || !t.text)) errs.push(`${at}: "text" is missing`);
    if (t.style === "counter" && !(Array.isArray(t.count) && t.count.length === 2 && t.count.every(num))) errs.push(`${at}: a counter needs "count": [from, to]`);
  } else if (kind === "graphics") {
    oneOf(t.type, Object.keys(GRAPHICS), at, "type"); oneOf(t.loop, LOOPS, at, "loop");
    if ((t.type === "emoji" || t.type === "emoji-rain") && !t.emoji) errs.push(`${at}: needs "emoji"`);
    if (t.type === "image" && !asset(t.image)) errs.push(`${at}: no such file ${t.image}`);
    if (t.seconds != null && t.end == null && num(t.start)) t.end = t.start + t.seconds;
    if (t.end == null && num(t.start) && GRAPHICS[t.type]) t.end = t.start + GRAPHICS[t.type];
    if (GRAPHICS[t.type] === null) { t.start ??= 0; t.end ??= t.time === "source" ? undefined : max; }
  } else {                                    // sounds
    if (t.at != null && t.start == null) t.start = t.at;
    t.end = (t.start ?? 0) + 0.1;
    if (!t.sfx && !t.src) errs.push(`${at}: needs "sfx" (a name) or "src" (a file)`);
    if (t.src && !asset(t.src)) errs.push(`${at}: no such file ${t.src}`);
  }
  if (!num(t.start) || !num(t.end) || t.end <= t.start) errs.push(`${at}: needs "start" < "end" in seconds`);
  else if (max != null && t.time !== "source" && t.start >= max) errs.push(`${at}: starts at ${t.start} s but ${where} is only ${max.toFixed(2)} s long`);
});
(plan.clips || []).forEach((c, i) => {
  const at = `clips[${i}]`;
  const kinds = ["src", "image", "card"].filter((k) => c[k] != null);
  if (kinds.length !== 1) return errs.push(`${at}: must have exactly one of "src", "image", "card"`);
  oneOf(c.fit, FITS, at, "fit"); oneOf(c.transition, TRANS, at, "transition"); oneOf(c.look, LOOKS, at, "look"); oneOf(c.backdrop, BACKDROPS, at, "backdrop");
  if (typeof c.motion === "string") c.motion = { type: c.motion };
  if (c.motion) { oneOf(c.motion.type, MOTIONS, at, "motion"); if (c.motion.amount != null && !(num(c.motion.amount) && c.motion.amount >= 1 && c.motion.amount <= 200)) errs.push(`${at}: motion amount must be 1-3 (how far it zooms)`); }
  if (c.inset != null && !(num(c.inset) && c.inset >= 0.3 && c.inset <= 0.98)) errs.push(`${at}: inset must be 0.3-0.98`);
  if (c.src != null) {
    if (!fs.existsSync(c.src)) return errs.push(`${at}: no such file ${c.src}`);
    let d; try { d = srcDuration(c.src); } catch { return errs.push(`${at}: ffprobe cannot read ${c.src}`); }
    c.start ??= 0; c.end ??= d;
    if (!num(c.start) || !num(c.end) || c.start < 0 || c.end <= c.start) errs.push(`${at}: needs 0 <= "start" < "end" (seconds of the source)`);
    else if (c.start >= d) errs.push(`${at}: start ${c.start} s is past the end of ${c.src} (${d.toFixed(2)} s)`);
    else if (c.end > d + 0.05) errs.push(`${at}: end ${c.end} s is past the end of ${c.src} (${d.toFixed(2)} s)`);
    c.end = Math.min(c.end, d);
    c.speed ??= 1; if (!num(c.speed) || c.speed < 0.25 || c.speed > 4) errs.push(`${at}: speed must be 0.25-4`);
    c.volume ??= 1; if (!num(c.volume) || c.volume < 0 || c.volume > 2) errs.push(`${at}: volume must be 0-2`);
    (c.captions || []).forEach((k, j) => {
      if (typeof k.text !== "string" || !num(k.start) || !num(k.end) || k.end <= k.start) errs.push(`${at} captions[${j}]: needs "text" and "start" < "end" (source seconds)`);
      else if (k.end <= c.start || k.start >= c.end) errs.push(`${at} captions[${j}]: ${k.start}-${k.end} s is outside the clip's ${c.start}-${c.end} s (captions use the source file's seconds)`);
    });
    (c.punches || []).forEach((k, j) => {
      if (!num(k.at) || k.at < c.start || k.at >= c.end) errs.push(`${at} punches[${j}]: "at" must be inside the clip's ${c.start}-${c.end} s (source seconds)`);
      k.scale ??= 1.3; k.seconds ??= 0.8;
      if (!num(k.scale) || k.scale < 1 || k.scale > 3) errs.push(`${at} punches[${j}]: scale must be 1-3`);
    });
  } else {
    if (!num(c.seconds) || c.seconds <= 0 || c.seconds > 600) errs.push(`${at}: needs "seconds" (how long it shows)`);
    if (c.image != null && !fs.existsSync(c.image)) errs.push(`${at}: no such file ${c.image}`);
    if (c.punches) errs.push(`${at}: punches belong to a video clip`);
  }
});
(Array.isArray(plan.html) ? plan.html : plan.html == null ? [] : (errs.push('"html" must be a list'), [])).forEach((b, j) => {
  if (typeof b.file !== "string" || !fs.existsSync(b.file)) errs.push(`html[${j}]: no such file ${b.file}`);
});
if (plan.music) {
  plan.music.src = asset(plan.music.src) || (errs.push(`music: no such file ${plan.music.src} (the library: edit assets music)`), null);
  if (plan.music.volume != null && !(num(plan.music.volume) && plan.music.volume >= 0 && plan.music.volume <= 1)) errs.push(`music: volume must be 0-1`);
}
if (errs.length) die("the plan has problems:\n  - " + errs.join("\n  - ") + "\n(`edit example` and `edit effects` show the format)");

// ---- the timeline in frames
const fr = (s) => Math.round(s * FPS);
const items = plan.clips.map((c) => {
  const seconds = c.src != null ? (c.end - c.start) / c.speed : c.seconds;
  return { c, frames: Math.max(1, fr(seconds)) };
});
items.forEach((it, i) => {
  const next = items[i + 1];
  it.tf = 0;
  if (it.c.transition && next) {
    it.tf = Math.max(1, fr(it.c.transitionSeconds ?? 0.4));
    const lim = Math.floor(Math.min(it.frames, next.frames) / 2);
    if (it.tf > lim) errs.push(`clips[${i}]: the transition (${(it.tf / FPS).toFixed(2)} s) is longer than half of the shorter of the two items it joins (${(lim / FPS).toFixed(2)} s)`);
  }
});
let pos = 0;
items.forEach((it) => { it.from = pos; pos += it.frames - it.tf; });
const total = pos;
items.forEach((it, i) => { for (const k of ["texts", "graphics", "sounds"]) checkTimed(it.c[k], `clips[${i}]`, it.frames / FPS, k); });
for (const k of ["texts", "graphics", "sounds"]) checkTimed(plan[k], "the video", total / FPS, k);
if (errs.length) die("the plan has problems:\n  - " + errs.join("\n  - "));

const captions = [], texts = [], graphics = [], sounds = [];
const auto = plan.sfx === "auto" || plan.sfx === true, sfxVol = num(plan.sfxVolume) ? plan.sfxVolume : 0.6;
const addSound = (name, frame, where, volume) => { const f = sfxFile(name, where); if (f) sounds.push({ from: Math.max(0, frame), file: f, volume: volume ?? sfxVol }); };
const place = (t, it) => {                   // an item's start/end -> output frames
  if (!it) return [fr(t.start), Math.min(total, fr(t.end))];
  const c = it.c, src = t.time === "source" && c.src != null;
  const a = src ? (t.start - c.start) / c.speed : t.start, b = src ? (t.end - c.start) / c.speed : t.end;
  return [it.from + Math.max(0, fr(a)), it.from + Math.min(it.frames, fr(b))];
};
const addText = (t, it, where) => {
  const [from, to] = place(t, it); if (to <= from) return;
  texts.push({ ...t, from, to });
  const anim = t.animate || TEXT_DEFAULT_ANIM[t.style] || "fade";
  if (t.sfx) addSound(t.sfx, from, where); else if (auto && t.sfx !== false && AUTO_TEXT_SFX[anim]) addSound(AUTO_TEXT_SFX[anim], from, where, sfxVol * 0.8);
};
const addGraphic = (g, it, where) => {
  const [from, to] = place(g, it); if (to <= from) return;
  const out = { ...g, from, to };
  if (g.emoji) { out.lottie = lottieFor(g.emoji); if (!out.lottie && /^[a-z0-9-]+$/.test(g.emoji)) errs.push(`${where}: no animated emoji named "${g.emoji}" (edit assets emoji)`); }
  if (g.type === "image") out.imageFile = path.resolve(asset(g.image));
  graphics.push(out);
  if (g.sfx) addSound(g.sfx, from, where); else if (auto && g.sfx !== false && AUTO_GFX_SFX[g.type]) addSound(AUTO_GFX_SFX[g.type], from, where, sfxVol * 0.8);
};
items.forEach((it, i) => {
  const c = it.c;
  for (const k of c.captions || []) {
    const a = it.from + Math.max(0, fr((k.start - c.start) / c.speed)), b = it.from + Math.min(it.frames, fr((k.end - c.start) / c.speed));
    if (b > a) captions.push({ from: a, to: b, text: k.text });
  }
  (c.texts || []).forEach((t, j) => addText(t, it, `clips[${i}] texts[${j}]`));
  (c.graphics || []).forEach((g, j) => addGraphic(g, it, `clips[${i}] graphics[${j}]`));
  (c.sounds || []).forEach((s, j) => { const [from] = place(s, it); if (s.src) sounds.push({ from, file: path.resolve(asset(s.src)), volume: s.volume ?? sfxVol }); else addSound(s.sfx, from, `clips[${i}] sounds[${j}]`, s.volume); });
  if (it.tf) {
    const t = c.transition, start = it.from + it.frames - it.tf;
    const name = c.sfx ?? (auto ? (AUTO_TRANS_SFX[t] || (/^(whip|slide)/.test(t) ? "whoosh" : null)) : null);
    if (name && c.sfx !== false) addSound(name, start - 2, `clips[${i}]`);
    if (legacyFlashy) graphics.push({ type: "flash", from: start + Math.round(it.tf / 2) - 2, to: start + Math.round(it.tf / 2) + 8 });
  }
});
(plan.texts || []).forEach((t, j) => addText(t, null, `texts[${j}]`));
(plan.graphics || []).forEach((g, j) => addGraphic(g, null, `graphics[${j}]`));
(plan.sounds || []).forEach((s, j) => { const [from] = place(s, null); if (s.src) sounds.push({ from, file: path.resolve(asset(s.src)), volume: s.volume ?? sfxVol }); else addSound(s.sfx, from, `sounds[${j}]`, s.volume); });
if (errs.length) die("the plan has problems:\n  - " + errs.join("\n  - "));
captions.sort((a, b) => a.from - b.from);
for (let i = 0; i + 1 < captions.length; i++) if (captions[i].to > captions[i + 1].from) captions[i].to = captions[i + 1].from;  // never two captions at once

const sec = (f) => (f / FPS).toFixed(2).padStart(7);
const table = () => {
  console.log(`timeline: ${W}x${H} at ${FPS} fps, ${(total / FPS).toFixed(2)} s, ${items.length} items, ${captions.length} captions, ${texts.length} texts, ${graphics.length} graphics, ${sounds.length} sound effects${plan.music ? ", music" : ""}${plan.theme ? `, theme ${plan.theme}` : ""}`);
  console.log("  #  output from-to (s)   item");
  items.forEach((it, i) => {
    const c = it.c;
    const what = c.src != null ? `${c.src} ${c.start}-${Number(c.end.toFixed(2))} s${c.speed !== 1 ? ` x${c.speed}` : ""}${c.volume !== 1 ? ` vol ${c.volume}` : ""} [${c.fit || "cover"}]${c.motion ? ` ${c.motion.type}` : ""}${c.punches?.length ? ` ${c.punches.length} punch` : ""}`
      : c.image != null ? `image ${c.image}` : `card "${String(c.card).replace(/\n/g, " / ")}"`;
    console.log(`${String(i).padStart(3)}  ${sec(it.from)} -${sec(it.from + it.frames)}   ${what}${it.tf ? `  → ${c.transition} ${(it.tf / FPS).toFixed(2)} s` : ""}`);
  });
};
if (cmd === "check") { table(); process.exit(0); }

// ---- prepare the sources with ffmpeg (cached): exact cut, plan size and fps, H.264 + stereo AAC
fs.mkdirSync(CACHE, { recursive: true });
const run = (args, what) => { const r = spawnSync("ffmpeg", ["-v", "error", "-y", ...args], { stdio: ["ignore", "inherit", "pipe"] }); if (r.status !== 0) die(`ffmpeg failed on ${what}:\n${r.stderr.toString().slice(-1500)}`); };
const fitFilter = (fit) => fit === "contain" ? `scale=${W}:${H}:force_original_aspect_ratio=decrease,pad=${W}:${H}:(ow-iw)/2:(oh-ih)/2:black`
  : fit === "blur" ? `split[a][b];[a]scale=${W}:${H}:force_original_aspect_ratio=increase,crop=${W}:${H},boxblur=30:4,eq=brightness=-0.08[bg];[b]scale=${W}:${H}:force_original_aspect_ratio=decrease[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2`
  : `scale=${W}:${H}:force_original_aspect_ratio=increase,crop=${W}:${H}`;
const atempo = (s) => { const f = []; while (s > 2) { f.push("atempo=2"); s /= 2; } while (s < 0.5) { f.push("atempo=0.5"); s /= 0.5; } f.push(`atempo=${s}`); return f.join(","); };
const ENC = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "14", "-pix_fmt", "yuv420p", "-g", String(Math.round(FPS)), "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", "-movflags", "+faststart"];
const key = (o) => createHash("sha1").update(JSON.stringify(o)).digest("hex").slice(0, 20);
items.forEach((it, i) => {
  const c = it.c;
  if (c.card != null) return;
  const src = path.resolve(c.src ?? c.image), st = fs.statSync(src);
  const out = path.join(CACHE, key([src, st.size, st.mtimeMs, c.start, c.end, c.speed, c.fit || "cover", c.seconds, W, H, FPS, 3]) + ".mp4");
  it.file = out;
  if (fs.existsSync(out)) return;
  const tmp = out + ".tmp.mp4", secs = it.frames / FPS;
  process.stderr.write(`preparing clip ${i}: ${path.basename(src)}\n`);
  if (c.image != null) {
    run(["-loop", "1", "-i", src, "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", String(secs), "-filter_complex", `[0:v]${fitFilter(c.fit)},setsar=1,fps=${FPS}[v]`, "-map", "[v]", "-map", "1:a", ...ENC, tmp], src);
  } else {
    const hasAudio = !!probe(src, "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0");
    const v = `[0:v:0]${fitFilter(c.fit)},setsar=1,setpts=(PTS-STARTPTS)/${c.speed},fps=${FPS}[v]`;
    const a = hasAudio ? `[0:a:0]${atempo(c.speed)},aresample=48000,asetpts=PTS-STARTPTS[a]` : null;
    const args = ["-ss", String(c.start), "-t", String(c.end - c.start), "-i", src];
    if (!hasAudio) args.push("-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo");
    args.push("-filter_complex", a ? `${v};${a}` : v, "-map", "[v]", "-map", a ? "[a]" : "1:a", "-t", String(secs), ...ENC, tmp);
    run(args, src);
  }
  fs.renameSync(tmp, out);
});

// ---- one sound bed for the music and every sound effect, mixed by ffmpeg (the footage keeps its own sound)
const m = plan.music;
const totalS = total / FPS;
let bed = null;
if (m || sounds.length) {
  bed = path.join(CACHE, key(["bed", m, sounds, total, FPS, items.map((it) => [it.from, it.frames, it.c.src == null || it.c.volume === 0])]) + ".wav");
  if (!fs.existsSync(bed)) {
    const args = [], chains = [], mix = [];
    if (m) {
      const vol = m.volume ?? 0.18, alone = m.duck === false ? vol : (m.volumeAlone ?? Math.min(1, vol * 4)), fo = Math.min(m.fadeOut ?? 1.5, totalS);
      // louder where nothing speaks (cards, silent clips), with a 0.4 s ramp either side
      const quiet = items.filter((it) => it.c.src == null || it.c.volume === 0).map((it) => [it.from / FPS, (it.from + it.frames) / FPS]);
      const k = quiet.length ? quiet.map(([x, y]) => `clip((min(t-${x.toFixed(3)},${y.toFixed(3)}-t)+0.2)/0.4,0,1)`).reduce((u, v) => `max(${u},${v})`) : "0";
      args.push("-stream_loop", "-1", "-ss", String(m.start ?? 0), "-i", path.resolve(m.src));
      chains.push(`[0:a]aresample=48000,aformat=channel_layouts=stereo,atrim=0:${totalS.toFixed(3)},asetpts=PTS-STARTPTS,volume='${vol}+(${alone}-${vol})*(${k})':eval=frame,afade=t=in:st=0:d=0.25,afade=t=out:st=${Math.max(0, totalS - fo).toFixed(3)}:d=${fo}[m]`);
      mix.push("[m]");
    }
    sounds.forEach((s2, i) => {
      const n = args.filter((x) => x === "-i").length;
      args.push("-i", s2.file);
      chains.push(`[${n}:a]aresample=48000,aformat=channel_layouts=stereo,volume=${s2.volume},adelay=${Math.round((s2.from / FPS) * 1000)}:all=1[s${i}]`);
      mix.push(`[s${i}]`);
    });
    chains.push(`${mix.join("")}amix=inputs=${mix.length}:normalize=0:duration=longest,atrim=0:${totalS.toFixed(3)},apad=whole_dur=${totalS.toFixed(3)}[out]`);
    run([...args, "-filter_complex", chains.join(";"), "-map", "[out]", "-ar", "48000", "-ac", "2", bed + ".tmp.wav"], "the music and sound effects");
    fs.renameSync(bed + ".tmp.wav", bed);
  }
}

// ---- the page: one HyperFrames project per plan (index.html, the composition's script, links to the media)
const esc = (v) => String(v).replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
const proj = path.join(CACHE, "page-" + key([path.resolve(planArg)]));
fs.rmSync(proj, { recursive: true, force: true });
fs.mkdirSync(path.join(proj, "media"), { recursive: true });
const place2 = (file) => {                  // a file into the project (a hard link when it can be, else a copy)
  const name = key([file]) + path.extname(file).toLowerCase(), to = path.join(proj, "media", name);
  if (!fs.existsSync(to)) { try { fs.linkSync(file, to); } catch { fs.copyFileSync(file, to); } }
  return "media/" + name;
};
const srcDir = path.join(HERE, "src");
const stamp = key(fs.readdirSync(srcDir).sort().map((f) => [f, fs.statSync(path.join(srcDir, f)).mtimeMs]));
const bundleFile = path.join(CACHE, `page-script-${stamp}.js`);
if (!fs.existsSync(bundleFile)) {
  process.stderr.write("building the composition…\n");
  const esbuild = await import("esbuild");
  const opts = { bundle: true, minify: true, format: "iife", target: "chrome120", define: { "process.env.NODE_ENV": '"production"' }, logLevel: "error" };
  await esbuild.build({ ...opts, entryPoints: [path.join(srcDir, "gsap-global.ts")], outfile: bundleFile + ".gsap.js" });
  await esbuild.build({ ...opts, entryPoints: [path.join(srcDir, "entry.tsx")], outfile: bundleFile + ".tmp" });
  fs.renameSync(bundleFile + ".tmp", bundleFile);
}
fs.copyFileSync(bundleFile, path.join(proj, "page.js"));
fs.copyFileSync(bundleFile + ".gsap.js", path.join(proj, "gsap.js"));
const FONT_FILES = { "Chiron Hei HK": "ChironHeiHK[wght].ttf", "Chiron GoRound TC": "ChironGoRoundTC[wght].ttf", "Chiron Sung HK": "ChironSungHK[wght].ttf", "LXGW WenKai TC": "LXGWWenKaiTC-Bold.ttf", Huninn: "Huninn-Regular.ttf", "Dela Gothic One": "DelaGothicOne-Regular.ttf", DotGothic16: "DotGothic16-Regular.ttf", Anton: "Anton-Regular.ttf", Poppins: "Poppins-Black.ttf", Bungee: "Bungee-Regular.ttf" };
const FONT_DIR = process.env.EDIT_FONTS || "/usr/local/share/fonts/edit";
const faces = Object.entries(FONT_FILES).filter(([, f]) => fs.existsSync(path.join(FONT_DIR, f)))
  .map(([fam, f]) => `@font-face { font-family: '${fam}'; src: url('${place2(path.join(FONT_DIR, f))}'); font-weight: ${/\[wght\]/.test(f) ? "100 900" : "400 900"}; }`).join("\n      ");
const pagePlan = {
  width: W, height: H, fps: FPS, totalFrames: total, captions, texts, captionStyle: plan.captionStyle || {},
  theme: plan.theme, accent: plan.accent, accent2: plan.accent2, font: plan.font, progressBar: plan.progressBar || false, lotties,
  graphics: graphics.map(({ imageFile, ...g }) => (imageFile ? { ...g, url: place2(imageFile) } : g)),
  items: items.map((it) => {
    const c = it.c, common = { from: it.from, frames: it.frames, transition: it.tf ? c.transition : null, transitionFrames: it.tf, transitionColor: c.transitionColor, background: c.background, backdrop: c.backdrop };
    if (c.card != null) return { ...common, kind: "card", title: String(c.card), subtitle: c.subtitle, color: c.color, emoji: c.emoji, lottie: c.emoji ? lottieFor(c.emoji) : null };
    return { ...common, kind: "video", motion: c.motion || null, look: c.look, inset: c.inset, insetY: c.insetY,
      punches: (c.punches || []).map((p) => ({ at: fr((p.at - c.start) / c.speed), frames: Math.max(4, fr(p.seconds / c.speed)), scale: p.scale, focus: p.focus })) };
  }),
};
const t3 = (f) => (f / FPS).toFixed(4);
const layers = items.map((it, i) => {
  const timing = `data-start="${t3(it.from)}" data-duration="${t3(it.frames)}" data-track-index="${i % 2}"`;
  if (it.c.card != null) return `<div class="item" id="item-${i}"><div class="fill clip" id="card-${i}" ${timing}></div></div>`;
  const vol = it.c.src != null ? it.c.volume ?? 1 : 0;
  const sound = vol > 0 ? `data-has-audio="true" data-volume="${vol}"` : "muted";
  return `<div class="item" id="item-${i}"><div class="fill" id="bg-${i}"></div><div class="fill frame" id="frame-${i}"><div class="fill" id="cam-${i}">`
    + `<video class="clip" id="v${i}" ${timing} data-media-start="0" ${sound} src="${esc(place2(it.file))}" playsinline></video></div><div class="fill vig" id="vig-${i}"></div></div></div>`;
});
// the agent's own HyperFrames blocks: files of HTML placed above the footage (plan "html": [{"file", "start", "end"}])
const blocks = (plan.html || []).map((b, j) => {
  const [from, to] = [fr(b.start ?? 0), Math.min(total, fr(b.end ?? totalS))];
  return `<div class="fill clip block" id="block-${j}" data-start="${t3(from)}" data-duration="${t3(to - from)}" data-track-index="${4 + j}">${fs.readFileSync(b.file, "utf8")}</div>`;
});
const html = `<!doctype html>
<html lang="zh-HK">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=${W}, height=${H}" />
    <style>
      ${faces}
      * { margin: 0; padding: 0; box-sizing: border-box; }
      html, body { width: ${W}px; height: ${H}px; overflow: hidden; background: #000; }
      #root { position: relative; width: ${W}px; height: ${H}px; overflow: hidden; background: #000; }
      .fill { position: absolute; top: 0; left: 0; width: 100%; height: 100%; }
      .item { position: absolute; top: 0; left: 0; width: 100%; height: 100%; overflow: hidden; visibility: hidden; }
      .frame { overflow: hidden; }
      .item video { width: 100%; height: 100%; object-fit: cover; display: block; }
      .vig { background: radial-gradient(ellipse at center, transparent 50%, rgba(0,0,0,.55) 100%); opacity: 0; }
      #fx { opacity: 0; pointer-events: none; }
    </style>
    <script src="gsap.js"></script>
  </head>
  <body>
    <div id="root" data-composition-id="main" data-start="0" data-duration="${t3(total)}" data-width="${W}" data-height="${H}" data-fps="${FPS}">
      ${layers.join("\n      ")}
      <div class="fill" id="fx"></div>
      ${blocks.join("\n      ")}
      <div class="fill" id="overlay"></div>
      ${bed ? `<audio class="clip" id="bed" data-start="0" data-duration="${t3(total)}" data-track-index="9" data-volume="1" src="${place2(bed)}"></audio>` : ""}
    </div>
    <script>window.__PLAN = ${JSON.stringify(pagePlan).replace(/</g, "\\u003c")};</script>
    <script src="page.js"></script>
  </body>
</html>
`;
fs.writeFileSync(path.join(proj, "index.html"), html);
fs.writeFileSync(path.join(proj, "hyperframes.json"), JSON.stringify({ name: "edit" }));

// ---- HyperFrames renders the page (headless Chrome, frame by frame) and mixes the sound
const HF = path.join(HERE, "node_modules", ".bin", "hyperframes");
// no telemetry, no agent-skill installs; the browser the installation pinned (EDIT_CHROME, or `chrome/` beside this
// file) instead of one HyperFrames downloads at first use
const chrome = process.env.EDIT_CHROME || path.join(HERE, "chrome", "chrome-headless-shell");
const hfEnv = { ...process.env, HYPERFRAMES_SKIP_SKILLS: "1", DO_NOT_TRACK: "1", HYPERFRAMES_NO_TELEMETRY: "1", ...(fs.existsSync(chrome) ? { PRODUCER_HEADLESS_SHELL_PATH: chrome } : {}) };
const hf = (args, what, quiet) => {
  const r = spawnSync(HF, args, { cwd: proj, env: hfEnv, stdio: ["ignore", "pipe", "pipe"], maxBuffer: 64 << 20 });
  if (r.status !== 0) die(`HyperFrames failed on ${what}:\n${(r.stdout.toString() + r.stderr.toString()).slice(-3000)}`);
  if (!quiet) for (const l of r.stderr.toString().split("\n")) if (/WARN|ERROR/i.test(l) && !/sparse keyframes|font/i.test(l)) process.stderr.write(l + "\n");
  return r.stdout.toString();
};
const stills = (times) => {                   // PNG stills at the given seconds, in order
  const dir = path.join(proj, "stills");
  fs.rmSync(dir, { recursive: true, force: true });
  hf(["snapshot", ".", "--at", times.map((t) => t.toFixed(3)).join(","), "--no-end", "--keep", "-o", dir, "--timeout", "120000"], "the stills", true);
  const have = fs.readdirSync(dir).filter((f) => /^at-.*\.png$/.test(f)).map((f) => ({ f: path.join(dir, f), t: parseFloat(f.slice(3)) }));
  return times.map((t) => { const hit = have.sort((x, y) => Math.abs(x.t - t) - Math.abs(y.t - t))[0]; if (!hit) die("HyperFrames wrote no still"); return hit.f; });
};
if (cmd === "frame") {
  const at = parseFloat(opt("--at") ?? "0");
  if (!isFinite(at) || at < 0 || fr(at) >= total) die(`--at must be 0-${((total - 1) / FPS).toFixed(2)} (output seconds)`);
  const out = path.resolve(opt("--out") || `work/frame-${at}.png`);
  fs.mkdirSync(path.dirname(out), { recursive: true });
  run(["-i", stills([at])[0], "-vf", `scale=-2:${Math.min(H, 960)}`, out], "the still");
  console.log(`${out}  (the finished video at ${at} s, scaled down; open it with read_file)`);
} else if (cmd === "sheet") {
  // several stills side by side with their times: the way to check motion (a text popping in, a punch-in, a transition)
  const a = parseFloat(opt("--from") ?? "0"), b = Math.min(parseFloat(opt("--to") ?? String(totalS)), (total - 1) / FPS);
  let every = parseFloat(opt("--every") ?? "0");
  if (!(b > a)) die(`--from must be before --to (the video is ${totalS.toFixed(2)} s)`);
  if (!every) every = Math.max(1 / FPS, (b - a) / 15);
  const times = []; for (let t = a; t <= b + 1e-6 && times.length < 24; t += every) times.push(t);
  const out = path.resolve(opt("--out") || "work/sheet.jpg"), tmp = fs.mkdtempSync(path.join(os.tmpdir(), "sheet-"));
  fs.mkdirSync(path.dirname(out), { recursive: true });
  stills(times).forEach((png, i) => run(["-i", png, "-vf", `scale=270:-2,drawtext=text='${times[i].toFixed(2)}s':x=6:y=6:fontsize=22:fontcolor=white:box=1:boxcolor=black@0.7:boxborderw=4`, path.join(tmp, `f${String(i).padStart(3, "0")}.png`)], "a sheet frame"));
  const cols = Math.min(times.length, 6), rows = Math.ceil(times.length / cols);
  run(["-framerate", "1", "-i", path.join(tmp, "f%03d.png"), "-vf", `tile=${cols}x${rows}:padding=4:color=black`, "-frames:v", "1", "-q:v", "3", out], "the sheet");
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log(`${out}  ${times.length} stills, left to right then down, every ${every.toFixed(2)} s from ${a} s (each is labelled with its time); open it with read_file`);
} else {
  table();
  const out = path.resolve(plan.output), raw = path.join(CACHE, `render-${process.pid}.mp4`);
  fs.mkdirSync(path.dirname(out), { recursive: true });
  process.stderr.write("rendering…\n");
  hf(["render", ".", "-o", raw, "-f", String(FPS), "--quiet"], "the render");
  if (plan.normalize === false) run(["-i", raw, "-c", "copy", "-movflags", "+faststart", out], "the final copy");
  else run(["-i", raw, "-c:v", "copy", "-af", "loudnorm=I=-16:TP=-1.5:LRA=11", "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-movflags", "+faststart", out], "the loudness pass");
  fs.rmSync(raw, { force: true });
  const d = parseFloat(probe(out, "-show_entries", "format=duration", "-of", "csv=p=0"));
  console.log(`done: ${out}  ${d.toFixed(2)} s, ${W}x${H}, ${(fs.statSync(out).size / 1e6).toFixed(1)} MB`);
}
process.exit(0);
