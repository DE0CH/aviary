#!/usr/bin/env python3
"""Temporary: local variants of a minimal installable page, one per port, to learn what Chrome on Android paints an
installed app's status bar with (manifest theme_color against the page's theme-color metas)."""
import http.server, json, os, sys, threading

PUBLIC = os.path.join(os.path.dirname(__file__), "../../../paseo/packages/app/public")
LIGHT, DARK = "(prefers-color-scheme: light)", "(prefers-color-scheme: dark)"
# name: (manifest theme_color, [(media or None, content)])
VARIANTS = {
    8107: ("TG", "#181b1a", [(LIGHT, "#f7f7f7"), (DARK, "#181b1a")]),
    8106: ("TF", "#181b1a", [(LIGHT, "#fafafa"), (DARK, "#181b1a")]),
    8104: ("TD", "#181b1a", [(LIGHT, "#eeeeef"), (DARK, "#181b1a")]),
    8102: ("TB", "#ffffff", [(LIGHT, "#ffffff"), (DARK, "#181b1a")]),
    8108: ("TH", "#f7f7f7", [(LIGHT, "#f7f7f7"), (DARK, "#181b1a")]),
}


def handler(name, theme, metas):
    tags = "".join(f'<meta name="theme-color" {f"media=\"{m}\" " if m else ""}content="{c}">' for m, c in metas)
    html = (f'<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">'
            f'{tags}<link rel="manifest" href="/manifest.json"><title>{name}</title>'
            f'<style>body{{background:#fff;color:#000}}@media (prefers-color-scheme: dark){{body{{background:#181b1a;color:#fff}}}}</style>'
            f'</head><body><h1>Ready {name}</h1></body></html>').encode()
    manifest = json.dumps({
        "id": "/", "name": name, "short_name": name, "start_url": "/", "scope": "/", "display": "standalone",
        "theme_color": theme, "background_color": "#181b1a",
        "icons": [{"src": f"/pwa-icon-{s}.png", "sizes": f"{s}x{s}", "type": "image/png", "purpose": "any maskable"}
                  for s in (192, 512)],
    }).encode()

    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_GET(self):
            if self.path == "/manifest.json":
                body, kind = manifest, "application/json"
            elif self.path.startswith("/pwa-icon-"):
                body, kind = open(os.path.join(PUBLIC, "branding", self.path.lstrip("/").replace("pwa-", "")), "rb").read(), "image/png"
            else:
                body, kind = html, "text/html"
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
    return H


def start():
    for port, (name, theme, metas) in VARIANTS.items():
        server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler(name, theme, metas))
        threading.Thread(target=server.serve_forever, daemon=True).start()


if __name__ == "__main__":
    start()
    threading.Event().wait()
