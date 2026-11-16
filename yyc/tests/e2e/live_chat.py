#!/usr/bin/env python3
"""YYC AI end to end on the live site (yyc-ai-e2e.yml, after each deploy): sign in as bot@deyaochen.com with Cloudflare
Access's e-mailed code, check /api/jarvis/me, start a conversation, wait for the model's answer, then type `/` and
need the list of the conversation's slash commands over the message box. Exits 1 with screenshots in OUT when any step
fails.

The code comes from the bot-mail Worker's /access-code route (claude-env email-bot skill), which serves only Access's
YYC AI code mail to the bot, behind Access with the service token yyc-ai-e2e (E2E_CODE_CLIENT_ID/SECRET).

  live_chat.py OUT [SITE]      (default https://ai.yyc-internal.com; needs playwright with chromium)"""
import json, os, sys, time, urllib.error, urllib.request
from datetime import datetime, timezone

CODE_URL = "https://bot-mail.deyaochen.com/access-code"
BOT = "bot@deyaochen.com"
PROMPT = "這是自動測試。請只回覆 4891 加 1 的結果（只寫數字）。"
EXPECT = "4892"


def fetch_code(after):
    req = urllib.request.Request(f"{CODE_URL}?after={after}", headers={
        "CF-Access-Client-Id": os.environ["E2E_CODE_CLIENT_ID"],
        "CF-Access-Client-Secret": os.environ["E2E_CODE_CLIENT_SECRET"], "User-Agent": "yyc-ai-e2e"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as err:
        try:
            return err.code, json.load(err)
        except ValueError:
            return err.code, {"error": f"HTTP {err.code}, not JSON (Access refused the service token?)"}


def wait_code(after, fetch=fetch_code, sleep=time.sleep, tries=45):
    for _ in range(tries):
        status, body = fetch(after)
        if status == 200 and body.get("code"):
            return body["code"]
        if status != 404:
            sys.exit(f"access-code route: {status} {body}")
        sleep(4)
    sys.exit(f"no Access code mail for {BOT} within {tries * 4} s")


def answered(text, prompt, expect):
    """the expected answer appears in the page after the prompt (the prompt itself does not contain it)"""
    i = text.rfind(prompt)
    return i >= 0 and expect in text[i + len(prompt):]


def command_names(text):
    """the commands a `/` list names: its rows start with the name (the highlighted command's card repeats one)"""
    names = []
    for line in text.split("\n"):
        name = line.strip()
        if name.startswith("/") and len(name) > 1 and " " not in name and name not in names:
            names.append(name)
    return names


def main():
    from playwright.sync_api import sync_playwright
    out = sys.argv[1]
    site = (sys.argv[2] if len(sys.argv) > 2 else "https://ai.yyc-internal.com").rstrip("/")
    os.makedirs(out, exist_ok=True)

    with sync_playwright() as p:
        b = p.chromium.launch()
        pg = b.new_context(viewport={"width": 1366, "height": 860}, locale="zh-HK").new_page()
        step = "start"

        def fail(msg):
            pg.screenshot(path=os.path.join(out, f"failed-{step}.png"))
            sys.exit(f"{step}: {msg}")

        step = "login"
        pg.goto(site + "/api/jarvis/signin", wait_until="domcontentloaded")
        pg.wait_for_timeout(3000)
        if "cloudflareaccess.com" not in pg.url:
            fail(f"expected Access's login page, got {pg.url.split('?')[0]}")
        after = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + "000Z"
        pg.fill("input[type=email], input[name=email]", BOT)
        pg.keyboard.press("Enter")
        step = "code"
        code = wait_code(after)
        pg.fill("input[name=code], input[type=text], input[inputmode=numeric]", code)
        pg.keyboard.press("Enter")
        pg.wait_for_url(lambda u: u.startswith(site), timeout=60000)
        print("signed in:", pg.url.split("?")[0])

        step = "me"
        r = pg.request.get(site + "/api/jarvis/me")
        if r.status != 200:
            fail(f"/api/jarvis/me {r.status} {r.text()[:200]}")
        print("/api/jarvis/me: 200")

        step = "send"
        box = pg.locator("textarea, [contenteditable=true]").first
        box.wait_for(timeout=60000)
        box.click()
        box.type(PROMPT)
        pg.keyboard.press("Enter")
        t0 = time.time()

        step = "reply"
        # a cold start boots an AWS host (~40-50 s) before the sandbox; 4 min covers it with room
        while time.time() - t0 < 240:
            pg.wait_for_timeout(3000)
            if answered(pg.inner_text("body"), PROMPT, EXPECT):
                break
        else:
            fail(f"no answer containing {EXPECT} within 240 s")
        print(f"answered in {round(time.time() - t0)} s: {pg.url.split('?')[0]}")
        pg.screenshot(path=os.path.join(out, "answered.png"))

        step = "commands"
        # the conversation's slash commands are known once its first reply has started
        box = pg.locator("textarea, [contenteditable=true]").first
        box.click()
        box.type("/")
        listed = pg.locator('[data-testid="composer-autocomplete-popover"]')
        t0, names = time.time(), []
        while time.time() - t0 < 30 and not names:
            pg.wait_for_timeout(1000)
            if listed.count():
                names = command_names(listed.first.inner_text())
        if not names:
            fail("no list of slash commands over the message box 30 s after typing /")
        print(f"/ lists {len(names)} commands: {' '.join(names[:5])} ...")
        pg.screenshot(path=os.path.join(out, "commands.png"))
        pg.keyboard.press("Backspace")
        b.close()


if __name__ == "__main__":
    main()
