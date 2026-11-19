#!/usr/bin/env python3
"""YYC Drive end to end on the live site (yyc-ai.yml job e2e, after each deploy): sign in as bot@deyaochen.com on YYC
AI's site with Access's e-mailed code (live_chat.py's sign-in: the code route serves only that site's mail), then open
the drive, which Access lets through on the same team sign-in, and need its file list. The page must come from the
edge (the Worker yyc-drive's static assets, DE0CH/yyc-drive YYC-DRIVE.md "The edge"), not from the pods. The time
from opening the drive to its file list is a notice on the run (annotations are readable without a login).
Exits 1 with a screenshot in OUT when a step fails.

  live_drive.py OUT [DRIVE] [SIGN_IN_SITE]   (defaults https://drive.yyc-internal.com, https://ai.yyc-internal.com)"""
import os, re, sys, time
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from live_chat import BOT, wait_code  # noqa: E402

# Cloudflare's own Cache-Control on a Worker's static asset that has no rule of its own (the page's index.html); the
# drive's server answered the page with "no-cache"
EDGE_PAGE = "public, max-age=0, must-revalidate"


def from_edge(headers):
    """Whether the drive's page came from the Worker's static assets (headers: the document's, lower-case names)."""
    return headers.get("cache-control", "") == EDGE_PAGE


def main():
    from playwright.sync_api import sync_playwright
    out = sys.argv[1]
    drive = (sys.argv[2] if len(sys.argv) > 2 else "https://drive.yyc-internal.com").rstrip("/")
    site = (sys.argv[3] if len(sys.argv) > 3 else "https://ai.yyc-internal.com").rstrip("/")
    os.makedirs(out, exist_ok=True)

    with sync_playwright() as p:
        b = p.chromium.launch()
        pg = b.new_context(viewport={"width": 1366, "height": 860}, locale="en-GB").new_page()
        step = "login"

        def fail(msg):
            pg.screenshot(path=os.path.join(out, f"drive-failed-{step}.png"))
            print(f"::error::YYC Drive live check, {step}: {msg}")
            sys.exit(1)

        pg.goto(site + "/api/jarvis/signin", wait_until="domcontentloaded")
        pg.wait_for_timeout(3000)
        if "cloudflareaccess.com" not in pg.url:
            fail(f"expected Access's login page, got {pg.url.split('?')[0]}")
        after = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + "000Z"
        pg.fill("input[type=email], input[name=email]", BOT)
        pg.keyboard.press("Enter")
        code = wait_code(after)
        pg.fill("input[name=code], input[type=text], input[inputmode=numeric]", code)
        pg.keyboard.press("Enter")
        pg.wait_for_url(lambda u: u.startswith(site), timeout=60000)

        step = "drive"
        t0 = time.monotonic()
        # Access's hops (the team's sign-in, /cdn-cgi/access/authorized) end on the page; goto answers its document
        doc = pg.goto(drive + "/", wait_until="domcontentloaded")
        if not pg.url.startswith(drive) or doc is None:
            fail(f"Access did not let the team's sign-in through to the drive: {pg.url.split('?')[0]}")
        if not from_edge(doc.headers):
            fail(f"the page did not come from the edge (Cache-Control {doc.headers.get('cache-control')!r})")

        step = "list"
        try:
            pg.get_by_text(re.compile(r"\bin total\b")).first.wait_for(timeout=60000)
        except Exception:
            fail("no file list within 60 s")
        took = time.monotonic() - t0
        pg.screenshot(path=os.path.join(out, "drive.png"))
        print(f"::notice::YYC Drive: the file list {took:.1f} s after opening the drive (signed in, from the runner)")
        b.close()


if __name__ == "__main__":
    main()
