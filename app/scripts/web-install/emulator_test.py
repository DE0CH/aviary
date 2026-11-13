#!/usr/bin/env python3
"""YYC AI installed from Chrome on an Android emulator (docs/paseo-ui.md "Installed on a phone").

There is no Android app: staff install the site from Chrome's menu. This drives that on the emulator's own Chrome,
against the deployed site ($YYC_SITE), with a screenshot of every step in OUT:
  1. the site loads signed out in Chrome;
  2. Chrome's menu offers to install it, and the install gives a home-screen app (a WebAPK);
  3. the installed app opens the site in its own window (no address bar);
  4. it turns with the phone (the manifest has no orientation lock);
  5. its status bar is the page's background in the light scheme and Chrome's own dark in the dark scheme;
  6. Sign in stays inside the installed app (Access's login in the app's window).
Run inside android-emulator-runner. Usage: emulator_test.py OUT

The repo is public: annotations carry fixed words and numbers only, never the site or anything read off the screen.
"""
import os
import re
import struct
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
import zlib

OUT = sys.argv[1]
SITE = os.environ["YYC_SITE"].rstrip("/")
CHROME = "com.android.chrome"
SIGN_IN = r"登入|Sign in"
os.makedirs(OUT, exist_ok=True)
step_no = [0]


def adb(*args, check=True, timeout=120):
    done = subprocess.run(["adb", *args], capture_output=True, timeout=timeout)
    if check and done.returncode != 0:
        raise RuntimeError(f"adb {args[0]} {args[1] if len(args) > 1 else ''} failed")
    return done.stdout


def sh(command, check=False):
    return adb("shell", command, check=check).decode("utf-8", "replace")


def note(text):
    print(text, flush=True)
    print(f"::notice title=install check::{text}", flush=True)


def shot(name):
    path = os.path.join(OUT, f"{name}.png")
    with open(path, "wb") as fh:
        fh.write(adb("exec-out", "screencap", "-p"))
    return path


KNOWN_SCREENS = {  # fixed words for what is on the screen at a failure (never anything read off the site)
    "an ANR dialog": r".*isn't responding.*",
    "Chrome's offline page": r".*(No internet|site can.t be reached|ERR_[A-Z_]+).*",
    "Chrome's Install dialog": r"Install app|Install",
    "the launcher's Add to home screen sheet": r"Add to home screen|Add automatically",
    "Sign in": SIGN_IN,
    "the home-screen icon": r"YYC AI",
}


def where():
    """The window in front and which known screens show, for a failure's annotation."""
    seen = nodes()
    top = re.search(r" (com\.[\w.]+)/([\w.$]+)", resumed())
    window = f"{top.group(1)}/{top.group(2).rsplit('.', 1)[-1]}" if top else "unknown"
    shows = [name for name, pattern in KNOWN_SCREENS.items()
             if any(re.fullmatch(pattern, node[key].strip(), re.S) for node in seen for key in ("text", "desc"))]
    return f"in front: {window}, {len(seen)} nodes, showing: {', '.join(shows) or 'none of the known screens'}"


def fail(name, text):
    shot(f"fail-{name}")
    save_ui(f"fail-{name}")
    # the device's log goes with the screenshots (encrypted in CI): what Chrome and the launcher said
    with open(os.path.join(OUT, f"fail-{name}.logcat.txt"), "wb") as fh:
        fh.write(adb("logcat", "-d", check=False))
    try:
        text = f"{text} ({where()})"
    except Exception:
        pass
    print(f"FAIL: {text}", file=sys.stderr, flush=True)
    print(f"::error title=install check::{name}: {text}", flush=True)
    sys.exit(1)


def nodes():
    """The screen's accessibility nodes, as dicts (text, desc, id, cls, bounds)."""
    sh("uiautomator dump /sdcard/ui.xml >/dev/null 2>&1")
    raw = adb("exec-out", "cat", "/sdcard/ui.xml", check=False)
    try:
        root = ET.fromstring(raw)
    except ET.ParseError:
        return []
    found = []
    for node in root.iter("node"):
        box = re.findall(r"\d+", node.get("bounds", ""))
        if len(box) != 4:
            continue
        found.append({
            "text": node.get("text", ""), "desc": node.get("content-desc", ""), "id": node.get("resource-id", ""),
            "cls": node.get("class", ""), "pkg": node.get("package", ""), "bounds": [int(v) for v in box],
        })
    return found


def save_ui(name):
    sh("uiautomator dump /sdcard/ui.xml >/dev/null 2>&1")
    with open(os.path.join(OUT, f"{name}.xml"), "wb") as fh:
        fh.write(adb("exec-out", "cat", "/sdcard/ui.xml", check=False))


def match(node, pattern, by=("text", "desc")):
    return any(re.fullmatch(pattern, node[key].strip()) for key in by)


def tap(node):
    x1, y1, x2, y2 = node["bounds"]
    sh(f"input tap {(x1 + x2) // 2} {(y1 + y2) // 2}")


# Dialogs that are not the site's or the check's: the emulator's "isn't responding", Chrome's first run and offers
NUISANCE = (r"Wait|Use without an account|No thanks|No, thanks|Not now|Got it|Accept & continue|Continue|"
            r"Don.t allow|Dismiss|Close|Skip")


def answer_anr(seen):
    """The emulator's "… isn't responding" dialog (its launcher, on a slow runner) covers everything: Wait."""
    if not any("isn't responding" in node["text"] for node in seen):
        return False
    wait = next((node for node in seen if node["text"] == "Wait"), None)
    if wait:
        tap(wait)
        time.sleep(3)
    return True


def wait_for(pattern, seconds, by=("text", "desc"), clear=True):
    """The first node matching `pattern` within `seconds`; nuisance dialogs are answered on the way."""
    end = time.time() + seconds
    while time.time() < end:
        seen = nodes()
        if answer_anr(seen):
            continue
        for node in seen:
            if match(node, pattern, by):
                return node
        if clear:
            for node in seen:
                if node["cls"].endswith("Button") and match(node, NUISANCE):
                    tap(node)
                    break
        time.sleep(2)
    return None


def resumed():
    for line in sh("dumpsys activity activities").splitlines():
        if "topResumedActivity" in line or "mResumedActivity" in line:
            return line.strip()
    return ""


def png_pixels(path):
    """(width, height, rows of RGB tuples) of an 8-bit RGB/RGBA PNG, without any library."""
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    pos, idat, width, height, channels = 8, b"", 0, 0, 0
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour = struct.unpack(">IIBB", body[:10])
            assert depth == 8 and colour in (2, 6)
            channels = 3 if colour == 2 else 4
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    raw = zlib.decompress(idat)
    stride = width * channels
    rows, prev = [], bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if kind == 1:
                line[i] = (line[i] + a) & 255
            elif kind == 2:
                line[i] = (line[i] + b) & 255
            elif kind == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif kind == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(line)
        prev = line
        if y > 200:  # only the top of the screen is ever read
            break
    return width, height, channels, rows


def png_size(path):
    with open(path, "rb") as fh:
        head = fh.read(24)
    return struct.unpack(">II", head[16:24])


def status_bar_colour(path):
    """The commonest colour of the status bar's row (icons and the clock are a minority of it)."""
    width, _height, channels, rows = png_pixels(path)
    counts = {}
    for y in (6, 10, 14):
        row = rows[y]
        for x in range(0, width, 3):
            pixel = tuple(row[x * channels:x * channels + 3])
            counts[pixel] = counts.get(pixel, 0) + 1
    return max(counts, key=counts.get)


def near(colour, want, slack=10):
    return all(abs(a - b) <= slack for a, b in zip(colour, want))


def webapks():
    return sorted(line.split(":", 1)[1].strip() for line in sh("pm list packages").splitlines()
                  if "webapk" in line.lower())


# ---- 0. Chrome, without its first-run screens
version = re.search(r"versionName=(\S+)", sh(f"dumpsys package {CHROME}"))
if not version:
    fail("0-chrome", "the emulator image has no Chrome")
note(f"Chrome {version.group(1)}, Android {sh('getprop ro.build.version.release').strip()}")
sh("echo 'chrome --disable-fre --no-default-browser-check --no-first-run' > /data/local/tmp/chrome-command-line")
sh(f"am set-debug-app --persistent {CHROME}")
sh("settings put system accelerometer_rotation 0; settings put system user_rotation 0; cmd uimode night no")
sh("logcat -G 32M")  # room for the whole run's log (fail() keeps it)
before = webapks()
# let a freshly booted emulator settle (its launcher's "isn't responding" comes in the first minute)
for _ in range(10):
    answer_anr(nodes())
    time.sleep(3)

# ---- 1. the site in Chrome
# a freshly booted emulator sometimes drops the first start (Chrome never comes up): ask again
loaded = None
for attempt in range(5):
    sh(f"am start -a android.intent.action.VIEW -d '{SITE}/' {CHROME} >/dev/null")
    loaded = wait_for(SIGN_IN, 50)
    if loaded:
        break
if not loaded:
    fail("1-site", "the signed-out site (Sign in) did not appear in Chrome")
shot("1-site-in-chrome")

# ---- 2. install from the menu
menu = wait_for(r".*menu_button", 20, by=("id",)) or wait_for(r"More options|Customize and control Google Chrome", 10)
if not menu:
    fail("2-menu", "Chrome's menu button was not found")
tap(menu)
INSTALL_ITEM = r"Install app|Add to Home screen|Add to home screen|Install .*"
item = wait_for(INSTALL_ITEM, 15, clear=False)
if not item:
    # the item can be below the fold of the menu
    width, height = png_size(shot("2-menu"))
    sh(f"input swipe {width // 2} {height * 3 // 4} {width // 2} {height // 4} 300")
    item = wait_for(INSTALL_ITEM, 10, clear=False)
shot("2-menu")
if not item:
    fail("2-menu", "Chrome's menu has neither Install app nor Add to Home screen")
label = "Install app" if item["text"].startswith("Install") else "Add to Home screen"
tap(item)
time.sleep(2)
shot("2-dialog")
save_ui("2-dialog")
# newer Chrome asks first: Install, or Create shortcut
taps = 0
for _ in range(3):
    button = wait_for(r"Install", 12, clear=False)
    if not button:
        break
    tap(button)
    taps += 1
    time.sleep(3)
    shot(f"2-dialog-after-{taps}")
if taps == 0:
    fail("2-install", f"after {label}, Chrome showed no Install button (it offers only a shortcut)")
# A phone signed in to Google gets a WebAPK (its own package). Without an account (this emulator) Chrome cannot have
# one minted and pins a home-screen shortcut instead, which opens the same standalone window from the same manifest.
package = None
pinned = False
for _ in range(40):
    new = [name for name in webapks() if name not in before]
    if new:
        package = new[0]
        break
    seen = nodes()
    if answer_anr(seen):
        continue
    pin = next((node for node in seen if match(node, r"Add to home screen|Add automatically|Add")
                and node["cls"].endswith("Button") and node["pkg"] != CHROME), None)
    if pin:
        tap(pin)
        pinned = True
        break
    # the sheet can come and go between two looks: the icon on the home screen says the same
    if any(match(node, r"YYC AI") and node["pkg"] != CHROME for node in seen):
        pinned = True
        break
    time.sleep(2)
if not package and not pinned:
    # no sheet seen: the launcher may have taken the icon by itself, onto its next page
    sh("input keyevent KEYCODE_HOME")
    time.sleep(2)
    for page in range(4):
        if wait_for(r"YYC AI", 6, clear=False):
            pinned = True
            break
        width, height = png_size(shot("2-home-page"))
        sh(f"input swipe {width * 4 // 5} {height // 2} {width // 5} {height // 2} 200")
if not package and not pinned:
    fail("2-install", f"{label} then Install ({taps} taps) gave neither an installed app nor a home-screen icon")
kind = "an app (WebAPK)" if package else "a home-screen icon (no Google account here, so no WebAPK)"
note(f"ok: Chrome's menu ({label}) installed the site as {kind}")
time.sleep(3)
sh("input keyevent KEYCODE_HOME")
time.sleep(3)
shot("2-home-screen")

# ---- 3. the installed app: its own window
sh(f"am force-stop {CHROME}")
if package:
    sh(f"monkey -p {package} -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1")
else:
    # the launcher puts the new icon on its next page and stays there; a second Home would go back to the first
    icon = wait_for(r"YYC AI", 10, clear=False)
    for _ in range(3):
        if icon:
            break
        width, height = png_size(shot("3-home-page"))
        sh(f"input swipe {width * 4 // 5} {height // 2} {width // 5} {height // 2} 200")
        icon = wait_for(r"YYC AI", 6, clear=False)
    if not icon:
        fail("3-app", "the home screen has no YYC AI icon after the install")
    tap(icon)
time.sleep(2)
shot("3-splash")
if not wait_for(SIGN_IN, 120):
    fail("3-app", "the installed app did not show the site (Sign in)")
time.sleep(2)
shot("3-app")
save_ui("3-app")
if any(node["id"].endswith(":id/url_bar") for node in nodes()):
    fail("3-app", "the installed app shows Chrome's address bar: not a standalone window")
note("ok: the installed app opens the site in its own window, no address bar")

# ---- 4. it turns with the phone
portrait = png_size(os.path.join(OUT, "3-app.png"))
sh("settings put system user_rotation 1")
time.sleep(5)
wait_for(SIGN_IN, 30)
landscape = png_size(shot("4-landscape"))
sh("settings put system user_rotation 0")
time.sleep(4)
if not (portrait[0] < portrait[1] and landscape[0] > landscape[1]):
    fail("4-rotate", f"the installed app stays portrait when the phone turns ({portrait[0]}x{portrait[1]} -> "
         f"{landscape[0]}x{landscape[1]}): the manifest locks the orientation")
note(f"ok: the installed app turns with the phone ({portrait[0]}x{portrait[1]} -> {landscape[0]}x{landscape[1]})")

# ---- 5. the status bar: the page's background in the light scheme, Chrome's own dark in the dark scheme
# What Chrome paints it with (measured on this emulator with local pages, 2026-11-13): the page's theme-color, unless
# that is brighter than about #eeeeee (#f7f7f7, #fafafa and #ffffff are ignored), then the manifest's theme_color;
# in the dark scheme its own #121318 whatever both say (the page's dark is #181b1a, next to it).
wait_for(SIGN_IN, 30)
wrong = []
for scheme, night, want in (("light", "no", (255, 255, 255)), ("dark", "yes", (0x12, 0x13, 0x18))):
    sh(f"cmd uimode night {night}")
    time.sleep(6)
    wait_for(SIGN_IN, 30)
    colour = status_bar_colour(shot(f"5-status-bar-{scheme}"))
    hexed = "#%02x%02x%02x" % colour
    if not near(colour, want):
        # a wrong colour does not stop the check: step 6 is worth knowing either way
        wrong.append(f"5-{scheme}")
        print(f"::error title=install check::5-{scheme}: the status bar is {hexed} in the {scheme} scheme, "
              f"not #%02x%02x%02x" % want, flush=True)
        continue
    note(f"ok: status bar {hexed} in the {scheme} scheme")
sh("cmd uimode night no")
time.sleep(4)

# ---- 6. Sign in stays in the installed app
button = wait_for(SIGN_IN, 30)
if not button:
    fail("6-signin", "Sign in is not on screen")
tap(button)
left = False
for _ in range(20):
    time.sleep(2)
    if not any(match(node, SIGN_IN) for node in nodes()):
        left = True
        break
time.sleep(6)
shot("6-signin")
save_ui("6-signin")
top = resumed()
if not left:
    fail("6-signin", "tapping Sign in did nothing")
if "webapp" not in top.lower() and "webapk" not in top.lower():
    fail("6-signin", "Sign in left the installed app for another window")
note("ok: Sign in opens Access's login inside the installed app")
if wrong:
    sys.exit(1)
note("ok: all steps")
