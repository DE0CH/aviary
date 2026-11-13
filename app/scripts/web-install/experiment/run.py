#!/usr/bin/env python3
"""Temporary: install each variant of server.py from Chrome on the emulator and report its status bar's colour in
the light and the dark scheme. Usage: run.py OUT"""
import os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import server as variants

source = open(os.path.join(HERE, "../emulator_test.py")).read()
os.environ.setdefault("YYC_SITE", "http://localhost")
exec(source[:source.index("# ---- 0. Chrome")])  # the helpers

variants.start()  # daemon threads: the step ends with this script
sh("echo 'chrome --disable-fre --no-default-browser-check --no-first-run' > /data/local/tmp/chrome-command-line")
sh(f"am set-debug-app --persistent {CHROME}")
sh("settings put system accelerometer_rotation 0; settings put system user_rotation 0; cmd uimode night no")
for _ in range(10):
    answer_anr(nodes())
    time.sleep(3)

def run(port, name, theme, metas):
    adb("reverse", f"tcp:{port}", f"tcp:{port}")
    ready = None
    for attempt in range(5):
        sh(f"am start -a android.intent.action.VIEW -d 'http://localhost:{port}/' {CHROME} >/dev/null")
        ready = wait_for(f"Ready {name}", 40)
        if ready:
            break
    if not ready:
        note(f"{name}: the page did not load")
        return
    time.sleep(3)
    menu = wait_for(r".*menu_button", 20, by=("id",))
    tap(menu)
    item = wait_for(r"Install app|Add to Home screen|Add to home screen", 15, clear=False)
    if not item:
        shot(f"{name}-no-item")
        note(f"{name}: no install item in the menu")
        sh("input keyevent KEYCODE_BACK")
        return
    tap(item)
    button = wait_for(r"Install|Add", 15, clear=False)
    if button:
        tap(button)
    pin = None
    for _ in range(30):
        seen = nodes()
        if answer_anr(seen):
            continue
        pin = next((n for n in seen if match(n, r"Add to home screen|Add automatically|Add")
                    and n["cls"].endswith("Button") and n["pkg"] != CHROME), None)
        if pin:
            tap(pin)
            break
        time.sleep(2)
    time.sleep(3)
    sh("input keyevent KEYCODE_HOME")
    time.sleep(2)
    sh(f"am force-stop {CHROME}")
    icon = wait_for(name, 10, clear=False)
    for _ in range(3):
        if icon:
            break
        width, height = png_size(shot("page"))
        sh(f"input swipe {width * 4 // 5} {height // 2} {width // 5} {height // 2} 200")
        icon = wait_for(name, 6, clear=False)
    if not icon:
        shot(f"{name}-no-icon")
        note(f"{name}: no icon on the home screen")
        return
    tap(icon)
    if not wait_for(f"Ready {name}", 40):
        shot(f"{name}-no-app")
        note(f"{name}: the installed app did not open")
        return
    found = {}
    for scheme, night in (("light", "no"), ("dark", "yes")):
        sh(f"cmd uimode night {night}")
        time.sleep(6)
        wait_for(f"Ready {name}", 20)
        found[scheme] = "#%02x%02x%02x" % status_bar_colour(shot(f"{name}-{scheme}"))
    sh("cmd uimode night no")
    note(f"{name}: manifest {theme}, metas {[c for _m, c in metas]} -> light {found['light']}, dark {found['dark']}")
    sh("input keyevent KEYCODE_HOME")
    time.sleep(2)


for port, (name, theme, metas) in variants.VARIANTS.items():
    try:
        run(port, name, theme, metas)
    except Exception as error:  # a dead emulator ends the run; anything else is that variant's
        note(f"{name}: {type(error).__name__}")
        if subprocess.run(["adb", "get-state"], capture_output=True).stdout.strip() != b"device":
            sys.exit(1)
