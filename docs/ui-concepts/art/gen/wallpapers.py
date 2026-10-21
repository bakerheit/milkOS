#!/usr/bin/env python3
# Host-only: needs Pillow (and numpy for wallpapers.py) to regenerate the art. Not needed to build or run milkOS.
"""milkOS wallpapers and boot splash, drawn by code (PIL and numpy only). Dawn pasture, Night pasture and Oat; a splash with the milk drop, light and night.

Every picture is drawn at 2048 x 1536 and downsampled, so the 1024 x 768 files are smooth. The top 28 px and the top-right corner stay calm (the menu bar, the toast, the drawer
and Today sit there), and the glass check at the end measures what the spec asks: primary text on a glass panel (the wallpaper box-blurred, the milk tint over it) over any part of
the picture.
"""
import math
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

W, H = 2048, 1536


def hexc(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    return tuple(int(round(a[i] * (1 - t) + b[i] * t)) for i in range(3))


MILK, CREAM, OAT, SAND, LATTE, COCOA, ESP = hexc("FFFDF8"), hexc("FBF6EC"), hexc("EFE6D4"), hexc("E2D5BD"), hexc("C4AE8F"), hexc("6B5846"), hexc("3A2E28")
STRAW, HONEY, MATCHA, BLUE, LAV, INK = hexc("E58C97"), hexc("EBB650"), hexc("9CBF9B"), hexc("7F93C9"), hexc("B9A9D6"), hexc("3A2E27")


def vgrad(w, h, stops):
    t = np.linspace(0, 1, h)[:, None]
    out = np.zeros((h, w, 3), np.float32)
    ts = [s[0] for s in stops]
    for c in range(3):
        out[:, :, c] = np.interp(t, ts, [s[1][c] for s in stops])
    return out


def to_img(a):
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8), "RGB").convert("RGBA")


def radial(w, h, cx, cy, r, color, alpha, power=2.0):
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    d = np.sqrt((xx - cx) ** 2 + (yy - cy) ** 2) / r
    a = np.clip(1 - d, 0, 1) ** power * alpha
    layer = np.zeros((h, w, 4), np.float32)
    layer[..., :3] = color
    layer[..., 3] = a * 255
    return Image.fromarray(layer.astype(np.uint8), "RGBA")


def blob(layer, cx, cy, rx, ry, color, alpha):
    d = ImageDraw.Draw(layer)
    d.ellipse([cx - rx, cy - ry, cx + rx, cy + ry], fill=color + (alpha,))


def hill(im, base, amp, freqs, phases, color, shade_to=None, top_light=0.18):
    xs = np.arange(0, W + 8, 8)
    ys = np.full(xs.shape, base * H, np.float32)
    for k, (f, ph) in enumerate(zip(freqs, phases)):
        ys += amp * H * np.sin(2 * math.pi * f * xs / W + ph) / (k + 1.2)
    mask = Image.new("L", (W, H), 0)
    ImageDraw.Draw(mask).polygon([(0, H)] + list(zip(xs.tolist(), ys.tolist())) + [(W, H)], fill=255)
    top = float(ys.min())
    g = vgrad(W, H, [(0, color), (min(1, (H - top) / H * 1.1) if False else 1.0, shade_to or color)])
    # lighter along the crest
    crest = np.clip(1.0 - (np.arange(H)[:, None, None] - top) / (0.16 * H), 0, 1)
    img = g + crest * top_light * (np.array([255, 253, 248], np.float32) - g)
    layer = to_img(img)
    layer.putalpha(mask)
    im.alpha_composite(layer)
    return xs, ys


def cow(im, cx, cy, s, flip=False, sleeping=False, tone=None):
    """A flat little cow: milk body, espresso patches, a pink snout. (cx, cy) is the middle of the hooves."""
    L = Image.new("RGBA", (int(120 * s), int(80 * s)), (0, 0, 0, 0))
    d = ImageDraw.Draw(L)
    k = s
    body = tone or MILK
    if sleeping:
        d.rounded_rectangle([8 * k, 34 * k, 86 * k, 70 * k], radius=16 * k, fill=body)
        d.ellipse([22 * k, 38 * k, 40 * k, 52 * k], fill=mix(INK, body, 0.15))
        d.ellipse([54 * k, 46 * k, 74 * k, 62 * k], fill=mix(INK, body, 0.15))
        d.rounded_rectangle([78 * k, 46 * k, 108 * k, 72 * k], radius=9 * k, fill=body)
        d.ellipse([100 * k, 60 * k, 110 * k, 70 * k], fill=STRAW)
        d.polygon([(82 * k, 46 * k), (88 * k, 34 * k), (94 * k, 47 * k)], fill=INK)
        d.arc([84 * k, 54 * k, 92 * k, 60 * k], 190, 350, fill=INK, width=max(1, int(1.5 * k)))
        d.line([(10 * k, 54 * k), (2 * k, 62 * k)], fill=INK, width=max(1, int(3 * k)))
    else:
        for lx in (20, 32, 62, 74):
            d.rectangle([lx * k, 50 * k, (lx + 7) * k, 74 * k], fill=INK)
        d.rounded_rectangle([10 * k, 22 * k, 90 * k, 60 * k], radius=14 * k, fill=body)
        d.ellipse([24 * k, 26 * k, 44 * k, 42 * k], fill=INK)
        d.ellipse([58 * k, 38 * k, 80 * k, 54 * k], fill=INK)
        d.rounded_rectangle([82 * k, 14 * k, 114 * k, 46 * k], radius=10 * k, fill=body)
        d.ellipse([102 * k, 30 * k, 114 * k, 44 * k], fill=STRAW)
        d.ellipse([92 * k, 20 * k, 98 * k, 26 * k], fill=INK)
        d.polygon([(84 * k, 16 * k), (88 * k, 6 * k), (94 * k, 16 * k)], fill=INK)
        d.line([(10 * k, 30 * k), (3 * k, 44 * k)], fill=INK, width=max(1, int(3 * k)))
    if flip:
        L = L.transpose(Image.Transpose.FLIP_LEFT_RIGHT)
    sh = Image.new("RGBA", L.size, (58, 46, 40, 255))
    sh.putalpha(L.split()[3].filter(ImageFilter.GaussianBlur(3 * s)).point(lambda v: v * 0.22))
    im.alpha_composite(sh, (int(cx - L.width / 2), int(cy - L.height * 0.9 + 6 * s)))
    im.alpha_composite(L, (int(cx - L.width / 2), int(cy - L.height * 0.9)))


def flowers(im, rng, n, y0, y1, colors, size):
    d = ImageDraw.Draw(im)
    for _ in range(n):
        x, y = rng.uniform(0, W), rng.uniform(y0, y1)
        c = colors[rng.integers(len(colors))]
        r = size * (0.6 + 0.8 * (y - y0) / (y1 - y0 + 1))
        d.ellipse([x - r, y - r, x + r, y + r], fill=c + (220,))


def grass_texture(im, rng, y0, color, alpha, n):
    layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    for _ in range(n):
        x, y = rng.uniform(0, W), rng.uniform(y0, H)
        l = 8 + 14 * (y / H)
        d.line([(x, y), (x + rng.uniform(-3, 3), y - l)], fill=color + (alpha,), width=3)
    im.alpha_composite(layer)


# ------------------------------------------------------------------------------------------------------------------------------ the pictures
def dawn():
    rng = np.random.default_rng(3)
    sky = vgrad(W, H, [(0.0, hexc("FBE3C8")), (0.45, hexc("F3C3B8")), (0.85, hexc("C9B3D9"))])
    im = to_img(sky)
    im.alpha_composite(radial(W, H, 0.745 * W, 0.215 * H, 0.30 * H, HONEY, 0.50, 2.2))
    im.alpha_composite(radial(W, H, 0.745 * W, 0.215 * H, 0.16 * H, hexc("FBE3C8"), 0.55, 1.4))
    sun = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    r = 0.095 * H
    ImageDraw.Draw(sun).ellipse([0.745 * W - r, 0.215 * H - r, 0.745 * W + r, 0.215 * H + r], fill=HONEY + (255,))
    im.alpha_composite(sun)
    clouds = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    for cx, cy, sc, a in ((0.20, 0.17, 1.0, 120), (0.46, 0.30, 0.8, 90), (0.10, 0.36, 0.7, 80)):
        for dx, dy, rx, ry in ((0, 0, 150, 38), (-90, 14, 100, 30), (100, 12, 110, 32), (30, -22, 90, 34)):
            blob(clouds, cx * W + dx * sc, cy * H + dy * sc, rx * sc, ry * sc, hexc("FFF4E6"), a)
    im.alpha_composite(clouds.filter(ImageFilter.GaussianBlur(14)))
    birds = ImageDraw.Draw(im)
    for bx, by, bs in ((0.31, 0.12, 1.0), (0.35, 0.145, 0.7), (0.285, 0.155, 0.6)):
        x, y, s = bx * W, by * H, 26 * bs
        birds.line([(x - s, y + s * 0.2), (x - s * 0.4, y - s * 0.25), (x, y + s * 0.1), (x + s * 0.4, y - s * 0.25), (x + s, y + s * 0.2)], fill=COCOA + (150,), width=int(5 * bs), joint="curve")
    hill(im, 0.66, 0.030, (1.1, 2.3), (0.4, 1.9), mix(LAV, MATCHA, 0.55), mix(LAV, MATCHA, 0.38), 0.14)
    hill(im, 0.745, 0.040, (0.9, 2.1), (2.2, 0.3), mix(MATCHA, MILK, 0.22), mix(MATCHA, MILK, 0.06), 0.18)
    xs, ys = hill(im, 0.865, 0.040, (0.8, 1.7), (4.0, 1.1), hexc("93B692"), hexc("84A883"), 0.16)
    grass_texture(im, rng, 0.80 * H, hexc("7FA27E"), 70, 2600)
    flowers(im, rng, 90, 0.90 * H, 0.99 * H, [STRAW, HONEY, MILK], 5)
    cow(im, 0.885 * W, 0.915 * H, 2.6)
    cow(im, 0.62 * W, 0.80 * H, 1.2, flip=True)
    return im.convert("RGB")


def night():
    rng = np.random.default_rng(5)
    sky = vgrad(W, H, [(0.0, hexc("231F3A")), (0.5, hexc("3F3560")), (0.85, hexc("6B557A"))])
    im = to_img(sky)
    mx, my, mr = 0.062 * W, 0.855 * H, 0.043 * H            # a moonrise at the bottom left, clear of every glass panel's region
    im.alpha_composite(radial(W, H, mx, my, 0.22 * H, hexc("F4EBE0"), 0.22, 2.4))
    im.alpha_composite(radial(W, H, mx, my, 0.11 * H, hexc("FFFDF8"), 0.26, 1.6))
    d = ImageDraw.Draw(im)
    d.ellipse([mx - mr, my - mr, mx + mr, my + mr], fill=hexc("FFF6E3") + (255,))
    for dx, dy, rr in ((-13, -8, 12), (10, 9, 8), (-3, 17, 6)):
        d.ellipse([mx + dx - rr, my + dy - rr, mx + dx + rr, my + dy + rr], fill=hexc("F0E2C4") + (255,))
    for _ in range(150):
        x, y = rng.uniform(0, 0.97 * W), rng.uniform(70, 0.5 * H)
        r = rng.choice([2, 2, 3, 4])
        a = int(rng.uniform(90, 230))
        d.ellipse([x - r, y - r, x + r, y + r], fill=hexc("FFF4E0") + (a,))
    for sx, sy in ((0.55, 0.14), (0.4, 0.26), (0.66, 0.10)):
        x, y = sx * W, sy * H
        d.polygon([(x, y - 14), (x + 4, y - 4), (x + 14, y), (x + 4, y + 4), (x, y + 14), (x - 4, y + 4), (x - 14, y), (x - 4, y - 4)], fill=hexc("FFF4E0") + (210,))
    hill(im, 0.67, 0.030, (1.1, 2.3), (0.4, 1.9), hexc("3B4A5A"), hexc("34425A"), 0.10)
    hill(im, 0.75, 0.040, (0.9, 2.1), (2.2, 0.3), hexc("2F4A4A"), hexc("284240"), 0.12)
    hill(im, 0.865, 0.040, (0.8, 1.7), (4.0, 1.1), hexc("27403C"), hexc("203733"), 0.10)
    grass_texture(im, rng, 0.80 * H, hexc("35544E"), 60, 2200)
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    for _ in range(26):
        x, y = rng.uniform(0.04 * W, 0.96 * W), rng.uniform(0.62 * H, 0.97 * H)
        gd.ellipse([x - 13, y - 13, x + 13, y + 13], fill=HONEY + (70,))
    im.alpha_composite(glow.filter(ImageFilter.GaussianBlur(9)))
    d = ImageDraw.Draw(im)
    for _ in range(26):
        pass
    cow(im, 0.885 * W, 0.935 * H, 2.4, sleeping=True, tone=hexc('978D98'))
    return im.convert("RGB")


def oat():
    rng = np.random.default_rng(7)
    a = vgrad(W, H, [(0.0, mix(OAT, MILK, 0.35)), (1.0, mix(OAT, SAND, 0.25))])
    n = rng.normal(0, 2.2, (H, W, 1)).astype(np.float32)
    im = to_img(a + n)
    # a very faint drop, large, low on the right: the milkOS mark, ten percent
    drop = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(drop)
    cx, cy, r = 1560, 1010, 250
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=MILK + (110,))
    d.polygon([(cx - r * 0.96, cy - r * 0.28), (cx, cy - r * 2.0), (cx + r * 0.96, cy - r * 0.28)], fill=MILK + (110,))
    im.alpha_composite(drop.filter(ImageFilter.GaussianBlur(3)))
    return im.convert("RGB")


# ------------------------------------------------------------------------------------------------------------------------------ the splash
def draw_drop(im, cx, cy, r, outline, fill, hl):
    """The pointed teardrop of the current build: a circle with straight sides up to a tip 2.05 radii above its centre, a thin outline, a small highlight."""
    t = math.asin(1 / 2.05)

    def shape(rr, mask):
        d = ImageDraw.Draw(mask)
        d.ellipse([cx - rr, cy - rr, cx + rr, cy + rr], fill=255)
        d.polygon([(cx, cy - 2.05 * rr), (cx + rr * math.cos(t), cy - rr * math.sin(t)), (cx - rr * math.cos(t), cy - rr * math.sin(t))], fill=255)

    outer, inner = Image.new("L", im.size, 0), Image.new("L", im.size, 0)
    shape(r, outer)
    shape(r * 0.90, inner)
    im.paste(Image.new("RGBA", im.size, outline + (255,)), (0, 0), outer)
    im.paste(Image.new("RGBA", im.size, fill + (255,)), (0, 0), inner)
    layer = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(layer).ellipse([cx - r * 0.52, cy - r * 0.10, cx - r * 0.34, cy + r * 0.44], fill=hl + (255,))
    im.alpha_composite(layer)


def wordmark(im, cx, base_y, h, ink, accent):
    """'milkOS' in round-capped strokes (own design, no font): the letters laid out on a baseline, h is the ascender height in pixels."""
    d = ImageDraw.Draw(im)
    st = max(2, int(h * 0.125))
    xh = 0.60 * h
    a = 0.40 * h
    gap = 0.26 * h

    def cap(x, y, col):
        d.ellipse([x - st / 2, y - st / 2, x + st / 2, y + st / 2], fill=col)

    def vline(x, y0, y1, col):
        d.line([(x, y0), (x, y1)], fill=col, width=st)
        cap(x, y0, col)
        cap(x, y1, col)

    x = 0.0
    items = []
    # m
    y_top = base_y - xh
    items.append(("m", x, 2 * a))
    x0 = cx_left = None
    letters = [("m", 2 * a), ("i", 0.0), ("l", 0.0), ("k", 0.78 * a), ("O", 0.78 * h), ("S", 0.60 * h)]
    total = sum(w for _, w in letters) + gap * (len(letters) - 1) + st
    x = cx - total / 2 + st / 2
    for name, w in letters:
        col = accent if name in ("O", "S") else ink
        if name == "m":
            vline(x, base_y, y_top + a / 2, col)
            vline(x + a, base_y, y_top + a / 2, col)
            vline(x + 2 * a, base_y, y_top + a / 2, col)
            d.arc([x, y_top, x + a, y_top + a], 180, 360, fill=col, width=st)
            d.arc([x + a, y_top, x + 2 * a, y_top + a], 180, 360, fill=col, width=st)
        elif name == "i":
            vline(x, base_y, y_top, col)
            cap(x, y_top - 0.30 * h * 0.8, col)
            r = st * 0.62
            d.ellipse([x - r, y_top - 0.32 * h - r, x + r, y_top - 0.32 * h + r], fill=col)
        elif name == "l":
            vline(x, base_y, base_y - h, col)
        elif name == "k":
            vline(x, base_y, base_y - h, col)
            d.line([(x, base_y - 0.36 * xh), (x + w, y_top)], fill=col, width=st)
            d.line([(x + 0.22 * w, base_y - 0.55 * xh), (x + w, base_y)], fill=col, width=st)
            cap(x + w, y_top, col)
            cap(x + w, base_y, col)
        elif name == "O":
            cap_h = 0.82 * h
            d.ellipse([x, base_y - cap_h, x + w, base_y], outline=col, width=st)
        elif name == "S":
            cap_h = 0.82 * h
            hh = cap_h / 2 + st * 0.2
            top = base_y - cap_h
            d.arc([x, top, x + w, top + hh], 90, 330, fill=col, width=st)
            d.arc([x, base_y - hh, x + w, base_y], 270, 360, fill=col, width=st)
            d.arc([x, base_y - hh, x + w, base_y], 0, 150, fill=col, width=st)
        x += w + gap


def splash(night_mode):
    bg = hexc("382D27") if night_mode else hexc("FBF6EC")
    im = Image.new("RGBA", (W, H), bg + (255,))
    k = W / 1024.0
    cx, cy, r = W / 2, 345 * k, 60 * k
    sh = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(sh).ellipse([cx - 75 * k, 415 * k, cx + 75 * k, 430 * k], fill=(0, 0, 0, 80) if night_mode else (150, 130, 100, 60))
    im.alpha_composite(sh.filter(ImageFilter.GaussianBlur(6 * k)))
    if night_mode:
        im.alpha_composite(radial(W, H, cx, cy, 190 * k, hexc("7F93C9"), 0.16, 2.0))
    blue = hexc("7F93C9")
    draw_drop(im, cx, cy, r, blue, hexc("FFFDF8") if not night_mode else hexc("F4EBE0"), hexc("C9D6EE") if not night_mode else hexc("B9C6E4"))
    wordmark(im, cx, 522 * k, 54 * k, hexc("F4EBE0") if night_mode else hexc("3A2E28"), blue)
    d = ImageDraw.Draw(im)
    track = hexc("55463C") if night_mode else hexc("E2D5BD")
    d.rounded_rectangle([cx - 110 * k, 557 * k, cx + 110 * k, 561 * k], radius=2 * k, fill=track + (255,))
    return im.convert("RGB")


def glass_report(img, dark):
    """The spec's glass: box blur 24 x3 of the 1024 picture, the milk tint at 0.55, then the primary text over it. Reports the worst ratio over the whole screen and the top-right."""
    small = img.resize((1024, 768), Image.Resampling.LANCZOS)
    blur = small
    for _ in range(3):
        blur = blur.filter(ImageFilter.BoxBlur(24))
    tint = np.array(hexc("2E2520") if dark else hexc("FFFDF8"), np.float32)
    g = np.asarray(blur).astype(np.float32) * 0.45 + tint * 0.55
    text = np.array(hexc("F4EBE0") if dark else hexc("3A2E28"), np.float32)

    def lum(c):
        c = c / 255.0
        c = np.where(c <= 0.03928, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)
        return 0.2126 * c[..., 0] + 0.7152 * c[..., 1] + 0.0722 * c[..., 2]

    lg, lt = lum(g), float(lum(text[None, :]))
    ratio = (np.maximum(lg, lt) + 0.05) / (np.minimum(lg, lt) + 0.05)
    return float(ratio.min()), float(ratio[0:200, 700:].min()), float(ratio[28:, :].min())


if __name__ == "__main__":
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name, fn, dark in (("wallpaper-dawn", dawn, False), ("wallpaper-night", night, True), ("wallpaper-oat", oat, False)):
        img = fn()
        img.save(out / f"{name}-2048x1536.png")
        img.resize((1024, 768), Image.Resampling.LANCZOS).save(out / f"{name}-1024x768.png")
        worst, tr, body = glass_report(img, dark)
        print(f"{name}: worst glass contrast over the screen {worst:.1f}:1, top-right {tr:.1f}:1, below the menu bar {body:.1f}:1")
    for name, nm in (("splash", False), ("splash-night", True)):
        img = splash(nm)
        img.save(out / f"{name}-2048x1536.png")
        img.resize((1024, 768), Image.Resampling.LANCZOS).save(out / f"{name}-1024x768.png")
    print("ok")
