#!/usr/bin/env python3
"""jarvis/chat-icon.py: draw Jarvis chat's icon -> jarvis/deploy/branding/ (the tab icons and the home-screen icons),
and with `--app-icon PATH` the iPhone app's icon too (app/packages/app/assets/images/jarvis-chat-icon.png).

The icon (Deyao, 2026-11-17): a chat bubble as the main mark, which tells the chat from the Jarvis 4 app, and the
Jarvis 4 ring as a badge at its bottom right. The bubble is a neon outline with three dots, in the ring's cyan. The
ring's geometry is DE0CH/jarvis4 app/scripts/make-icon.py's. A tab icon's status dot (a reply runs: blue; a
conversation waits for the person: amber) sits at the top right, since the badge has the bottom right.

Everything is drawn as shapes (no font file) at 2048 px and scaled down. Run it, then jarvis/branding.py, and commit
both; logo.png (the logo inside the app) is not drawn here."""
import argparse, math, os
from PIL import Image, ImageChops, ImageDraw, ImageFilter

S = 2048
NAVY = (8, 14, 26); CYAN = (110, 225, 255); CORE = (200, 245, 255)
BLUE = (59, 130, 246); AMBER = (245, 158, 11)
BRANDING = os.path.join(os.path.dirname(os.path.abspath(__file__)), "deploy", "branding")


def blank():
    return Image.new("L", (S, S), 0)


def disc(cx, cy, r):
    m = blank(); ImageDraw.Draw(m).ellipse([cx - r, cy - r, cx + r, cy + r], fill=255)
    return m


def ring_masks(cx, cy, R):
    """The Jarvis 4 ring with outer radius R: (the rings and coil segments, the "4")."""
    k = R / 720
    m = blank(); d = ImageDraw.Draw(m)
    for r, w in ((720, 60), (430, 40)):
        d.ellipse([cx - r * k, cy - r * k, cx + r * k, cy + r * k], outline=255, width=max(2, int(w * k)))
    for i in range(10):
        a0 = i * 36 + 4
        d.arc([cx - 600 * k, cy - 600 * k, cx + 600 * k, cy + 600 * k], a0, a0 + 28, fill=255, width=max(2, int(120 * k)))
    four = blank(); f = ImageDraw.Draw(four)
    w = 110 * k
    sx, top, bot = cx + 85 * k, cy - 285 * k, cy + 285 * k
    bx0, by, bx1 = cx - 215 * k, cy + 115 * k, cx + 210 * k
    def seg(p, q):
        n = int(math.dist(p, q) / 2) + 1
        return [(p[0] + (q[0] - p[0]) * t / n, p[1] + (q[1] - p[1]) * t / n) for t in range(n + 1)]
    for x, y in seg((sx, top), (bx0, by)) + seg((bx0, by), (bx1, by)) + seg((sx, top), (sx, bot)):
        f.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=255)
    return m, four


def outline(box, radius, tail, width):
    """The outline of a rounded rectangle with a tail at its bottom edge."""
    m = blank(); d = ImageDraw.Draw(m)
    d.rounded_rectangle(box, radius=radius, fill=255)
    d.polygon(tail, fill=255)
    inner = m.filter(ImageFilter.GaussianBlur(width / 2)).point(lambda v: 255 if v >= 249 else 0)
    return ImageChops.subtract(m, inner.filter(ImageFilter.GaussianBlur(1.5)))


def dots(box, rad):
    m = blank(); d = ImageDraw.Draw(m)
    cx, cy = (box[0] + box[2]) / 2, (box[1] + box[3]) / 2
    for i in (-1, 0, 1):
        x = cx + i * rad * 3.4
        d.ellipse([x - rad, cy - rad, x + rad, cy + rad], fill=255)
    return m


class Layer:
    def __init__(self):
        self.img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    def paint(self, mask, colour, glow=0):
        if glow:
            halo = mask.filter(ImageFilter.GaussianBlur(glow)).point(lambda v: int(v * 0.6))
            self.img.alpha_composite(Image.merge("RGBA", [Image.new("L", (S, S), c) for c in CYAN] + [halo]))
        self.img.alpha_composite(Image.merge("RGBA", [Image.new("L", (S, S), c) for c in colour] + [mask]))

    def clear(self, mask):
        self.img.putalpha(ImageChops.multiply(self.img.getchannel("A"), ImageChops.invert(mask)))


def mark(box, radius, tail, stroke, dot, badge, gap, glow):
    """The bubble and its badge on a transparent layer."""
    layer = Layer()
    layer.paint(outline(box, radius, tail, stroke), CYAN, glow)
    layer.paint(dots(box, dot), CORE, glow)
    bx, by, br, ring_r = badge
    layer.clear(disc(bx, by, br + gap))
    layer.paint(disc(bx, by, br), NAVY)
    rings, four = ring_masks(bx, by, ring_r)
    layer.paint(rings, CYAN, glow / 2)
    layer.paint(four, CORE)
    return layer


def tile(scale=1.0):
    """The square icon: the mark on the Jarvis 4 app's dark tile. scale < 1 shrinks the mark about the centre (a
    maskable icon keeps it inside the middle 80%)."""
    bg = Image.new("RGB", (S, S)); d = ImageDraw.Draw(bg); c = S / 2
    for r in range(int(S * 0.75), 0, -8):
        u = (1 - r / (S * 0.75)) ** 2
        d.ellipse([c - r, c - r, c + r, c + r], fill=(int(8 + 22 * u), int(14 + 50 * u), int(26 + 70 * u)))
    fg = mark([330, 400, 1640, 1330], 250, [(520, 1300), (520, 1590), (860, 1300)], 96, 78, (1500, 1470, 330, 330), 70, 40).img
    n = int(S * scale); off = (S - n) // 2
    out = bg.convert("RGBA")
    out.alpha_composite(fg.resize((n, n), Image.LANCZOS), (off, off))
    return out.convert("RGB")


def tab(state=None):
    """The tab icon: the mark alone on a transparent ground, filling its square."""
    layer = mark([60, 110, 1820, 1400], 330, [(300, 1360), (300, 1800), (780, 1360)], 170, 120, (1500, 1500, 520, 437), 80, 0)
    if state:
        layer.clear(disc(1640, 400, 470))
        layer.paint(disc(1640, 400, 400), BLUE if state == "running" else AMBER)
    return layer.img


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--app-icon", help="also write the iPhone app's icon (1024 px, no alpha) here")
    ap.add_argument("--out", default=BRANDING, help="where the branding files go")
    a = ap.parse_args()
    def save(img, name, px):
        img.resize((px, px), Image.LANCZOS).save(os.path.join(a.out, name), optimize=True); print("wrote", name)
    save(tab(), "favicon.png", 64)
    save(tab("running"), "favicon-running.png", 64)
    save(tab("attention"), "favicon-attention.png", 64)
    full = tile()
    save(full, "apple-touch-icon.png", 180)
    maskable = tile(0.8)
    save(maskable, "icon-192.png", 192)
    save(maskable, "icon-512.png", 512)
    if a.app_icon:
        full.resize((1024, 1024), Image.LANCZOS).save(a.app_icon, optimize=True); print("wrote", a.app_icon)
