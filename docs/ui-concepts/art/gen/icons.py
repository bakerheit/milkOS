#!/usr/bin/env python3
# Host-only: needs Pillow (and numpy for wallpapers.py) to regenerate the art. Not needed to build or run milkOS.
"""milkOS app icons, drawn by code (PIL only, no outside art or fonts).

A flat glyph on a rounded tile (radius 24% of the size) on a 48-unit grid, to the UI spec's tokens. Rules this follows: glyph about 55 to 60% of the tile, strokes at
least 2 units (2 px at 48), glyph INK (#3A2E27) on accent tiles in both themes, espresso on neutral tiles (and #F4EBE0 in the dark theme), at most one small accent
per icon, a faint top highlight and a soft shadow under the glyph, no gradient across the tile. Terminal and Activity keep the fixed Terminal body colour in both
themes. Outputs: <name>-<size>.png and <name>-<size>-dark.png (identical for accent tiles) for 512, 96, 48, 40, 36, 28, 20, 16.
"""
import math
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

MASTER = 2048
U = MASTER / 48.0
BAKE_BORDER = False        # the shell draws the light tiles' 1px sand edge (the spec's icon code); pass --border to bake it into the PNG
BOLD = 1.0               # stroke multiplier: the small sizes are drawn with heavier strokes (hand-tuned, not just scaled down)


def hexc(h):
    h = h.lstrip("#")
    return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))


def mix(a, b, t):
    return tuple(int(round(a[i] * (1 - t) + b[i] * t)) for i in range(3))


STRAW, HONEY, MATCHA, BLUE, LAV = hexc("E58C97"), hexc("EBB650"), hexc("9CBF9B"), hexc("7F93C9"), hexc("B9A9D6")
INK, TERM, TERM_TEXT, TERM_BAR = hexc("3A2E27"), hexc("3A2E29"), hexc("FBF6ED"), hexc("4A3C34")
LIGHT = dict(milk=hexc("FFFDF8"), cream=hexc("FBF6EC"), oat=hexc("EFE6D4"), sand=hexc("E2D5BD"), latte=hexc("C4AE8F"), cocoa=hexc("6B5846"), esp=hexc("3A2E28"))
DARK = dict(milk=hexc("2E2520"), cream=hexc("382D27"), oat=hexc("42352D"), sand=hexc("55463C"), latte=hexc("8C7764"), cocoa=hexc("BEA890"), esp=hexc("F4EBE0"))


class Glyph:
    def __init__(self):
        self.im = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
        self.d = ImageDraw.Draw(self.im)

    def rr(self, x, y, w, h, r, fill=None, outline=None, width=0):
        self.d.rounded_rectangle([x * U, y * U, (x + w) * U, (y + h) * U], radius=r * U, fill=fill, outline=outline, width=int(width * BOLD * U))

    def ell(self, cx, cy, rx, ry, fill=None, outline=None, width=0):
        self.d.ellipse([(cx - rx) * U, (cy - ry) * U, (cx + rx) * U, (cy + ry) * U], fill=fill, outline=outline, width=int(width * BOLD * U))

    def poly(self, pts, fill):
        self.d.polygon([(x * U, y * U) for x, y in pts], fill=fill)

    def line(self, pts, width, fill, caps=True):
        pp = [(x * U, y * U) for x, y in pts]
        self.d.line(pp, fill=fill, width=int(width * BOLD * U), joint="curve")
        if caps:
            r = width * BOLD * U / 2
            for x, y in (pp[0], pp[-1]):
                self.d.ellipse([x - r, y - r, x + r, y + r], fill=fill)

    def rot_ell(self, cx, cy, rx, ry, ang, fill):
        s = Image.new("L", (int(rx * 2.6 * U), int(rx * 2.6 * U)), 0)
        c = s.width / 2
        ImageDraw.Draw(s).ellipse([c - rx * U, c - ry * U, c + rx * U, c + ry * U], fill=255)
        s = s.rotate(ang, resample=Image.Resampling.BICUBIC)
        layer = Image.new("RGBA", s.size, fill + (255,))
        layer.putalpha(s)
        self.im.alpha_composite(layer, (int(cx * U - s.width / 2), int(cy * U - s.height / 2)))


def tile_mask():
    m = Image.new("L", (MASTER, MASTER), 0)
    ImageDraw.Draw(m).rounded_rectangle([0, 0, MASTER - 1, MASTER - 1], radius=int(MASTER * 0.24), fill=255)
    return m


def compose(tile_fill, glyphs, border=None, dark_shadow=False, band=None, edge=None):
    """Tile, a faint top light, an optional band, the glyph with its soft shadow, all clipped to the tile."""
    base = Image.new("RGBA", (MASTER, MASTER), tile_fill + (255,))
    light = Image.new("L", (MASTER, MASTER), 0)
    gl = ImageDraw.Draw(light)
    n = int(MASTER * 0.5)
    for y in range(n):
        gl.line([(0, y), (MASTER, y)], fill=int(34 * (1 - y / n) ** 1.7))
    base.alpha_composite(Image.merge("RGBA", (Image.new("L", base.size, 255),) * 3 + (light,)))
    if band is not None:
        base.alpha_composite(band)
    for g in glyphs:
        a = g.im.split()[3]
        col = (12, 8, 6) if dark_shadow else INK
        sh = Image.new("RGBA", g.im.size, col + (255,))
        sm = ImageChops.offset(a, 0, int(0.8 * U)).filter(ImageFilter.GaussianBlur(0.8 * U))
        sh.putalpha(ImageChops.multiply(sm, Image.new("L", sm.size, 36 if dark_shadow else 30)))
        base.alpha_composite(sh)
        base.alpha_composite(g.im)
    out = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    out.paste(base, (0, 0), tile_mask())
    if edge is not None:
        border = edge
    elif not BAKE_BORDER:
        border = None
    if border is not None:
        ring = Image.new("L", (MASTER, MASTER), 0)
        ImageDraw.Draw(ring).rounded_rectangle([0, 0, MASTER - 1, MASTER - 1], radius=int(MASTER * 0.24), outline=255, width=max(2, int(U)))
        out.alpha_composite(Image.merge("RGBA", tuple(Image.new("L", out.size, c) for c in border) + (ring,)))
    return out


def band_top(color, height):
    b = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    ImageDraw.Draw(b).rectangle([0, 0, MASTER, int(height * U)], fill=color + (255,))
    return b


# ------------------------------------------------------------------------------------------------------------------------------ the twelve
def files(t):
    g = Glyph()
    g.rr(9.5, 12.5, 13.5, 9, 3, INK)
    g.rr(9.5, 15.5, 29, 21.5, 3.5, INK)
    g.rr(14, 17.4, 20, 9, 1.6, hexc("FFFDF8"))                        # the accent: a sheet of paper
    g.rr(9.5, 21.4, 29, 16.5, 3.5, HONEY)                             # a honey hairline between the back and the front
    g.rr(9.5, 23, 29, 14.4, 3.5, INK)
    return compose(HONEY, [g])


def terminal(t):
    g = Glyph()
    g.line([(12.5, 16), (21.5, 24), (12.5, 32)], 3.6, TERM_TEXT)
    g.rr(25, 30.2, 11, 3.6, 1.8, HONEY)                               # the accent: the cursor
    return compose(TERM, [g], dark_shadow=True, edge=TERM_BAR if t is DARK else None)


def browser(t):
    g = Glyph()
    g.ell(24, 24, 13.6, 13.6, outline=INK, width=2.5)
    if BOLD < 1.5:
        g.ell(24, 24, 5.8, 13.6, outline=INK, width=2.1)
    g.line([(24, 10.4), (24, 37.6)], 2.1, INK, False)
    g.line([(10.4, 24), (37.6, 24)], 2.1, INK, False)
    return compose(BLUE, [g])


def notes(t):
    g = Glyph()
    for y, w in (((24, 22), (30, 22), (36, 13)) if BOLD < 1.5 else ((26, 22), (35, 22))):
        g.line([(13, y), (13 + w, y)], 2.4, t["esp"], False)
    return compose(t["milk"], [g], border=t["sand"], band=band_top(HONEY, 14.5))


def paint(t):
    g = Glyph()
    pts = []
    for k in range(72):
        a = 2 * math.pi * k / 72
        pts.append((24 + 15.2 * math.cos(a) * (1 + 0.05 * math.cos(3 * a)), 24.6 + 12.4 * math.sin(a) * (1 - 0.07 * math.sin(2 * a))))
    g.d.line([(x * U, y * U) for x, y in pts + [pts[0]]], fill=t["esp"], width=int(2.3 * U), joint="curve")
    g.ell(18.4, 30.4, 2.5, 2.2, outline=t["esp"], width=1.8)
    for (x, y, c) in ((15.5, 21.6, STRAW), (22.6, 17.2, HONEY), (30.4, 19.2, MATCHA), (34, 26.4, BLUE)):
        g.ell(x, y, 3.2, 3.0, fill=c)
    return compose(t["milk"], [g], border=t["sand"])


def music(t):
    g = Glyph()
    for cx, cy in ((17.2, 33.8), (31.4, 31.2)):
        g.rot_ell(cx, cy, 5.4, 4.0, 22, INK)
    g.rr(20.2, 14.2, 2.6, 20, 0.9, INK)
    g.rr(34.4, 11.6, 2.6, 20, 0.9, INK)
    g.poly([(20.2, 14.2), (37, 11.0), (37, 17.0), (20.2, 20.2)], INK)
    return compose(STRAW, [g])


def settings(t):
    g = Glyph()
    pts = []
    small = BOLD >= 1.5
    n = 6 if small else 8
    r_out, r_in = (16.2, 10.6) if small else (14.4, 11.4)
    for k in range(n):
        c = 2 * math.pi * k / n
        wide = 0.34 if small else 0.30
        for da, r in ((-wide * 1.5, r_in), (-wide * 0.62, r_out), (wide * 0.62, r_out), (wide * 1.5, r_in)):
            pts.append((24 + r * math.cos(c + da), 24 + r * math.sin(c + da)))
    g.poly(pts, t["esp"])
    g.ell(24, 24, r_in + 0.4, r_in + 0.4, fill=t["esp"])
    g.ell(24, 24, 5.4 if small else 4.7, 5.4 if small else 4.7, fill=t["latte"])
    return compose(t["latte"], [g])


def text_editor(t):
    g = Glyph()
    for y, w in (((15, 24), (21, 24), (27, 16), (33, 20)) if BOLD < 1.5 else ((16, 24), (26, 24), (36, 14))):
        g.line([(12, y), (12 + w, y)], 2.4, t["cocoa"], False)
    g.rr(33.4 if BOLD < 1.5 else 29.6, 29.4 if BOLD < 1.5 else 31, 2.6, 8.6, 1.3, STRAW)                            # the accent: a caret
    return compose(t["milk"], [g], border=t["sand"])


DIGITS = {"2": [(0.8, 4.4), (1.8, 1.8), (4.2, 0.8), (6.6, 1.8), (7.4, 4.4), (6.6, 7.0), (0.8, 14.0), (7.8, 14.0)],
          "1": [(1.0, 3.6), (4.4, 0.8), (4.4, 14.0)]}


def calendar(t):
    g = Glyph()
    x0 = 24 - (8 + 3 + 8) / 2 + 0.2
    for k, ch in enumerate("21"):
        pts = [(x0 + k * 11 + x, 20.2 + y) for x, y in DIGITS[ch]]
        g.line(pts, 2.8, t["esp"])
    for x in (15.5, 32.5):
        g.rr(x - 1.4, 3.6, 2.8, 7.4, 1.4, t["milk"])                  # the rings on the band
    return compose(t["milk"], [g], border=t["sand"], band=band_top(STRAW, 15.5))


def clock(t):
    g = Glyph()
    g.ell(24, 24, 13.6, 13.6, outline=INK, width=2.5 if BOLD < 1.5 else 2.0)
    for k in range(4 if BOLD < 1.5 else 0):
        a = math.radians(k * 90 - 90)
        g.line([(24 + 10.4 * math.cos(a), 24 + 10.4 * math.sin(a)), (24 + 12.2 * math.cos(a), 24 + 12.2 * math.sin(a))], 2.2, INK, False)
    hl, ml = (6.4, 9.6) if BOLD < 1.5 else (7.2, 11.6)
    g.line([(24, 24), (24 + hl * math.cos(math.radians(-60)), 24 + hl * math.sin(math.radians(-60)))], 2.7 if BOLD < 1.5 else 2.2, INK)
    g.line([(24, 24), (24 + ml * math.cos(math.radians(60)), 24 + ml * math.sin(math.radians(60)))], 2.3 if BOLD < 1.5 else 1.9, INK)
    if BOLD < 1.5:
        g.line([(24 - 2.6 * math.cos(math.radians(200)), 24 - 2.6 * math.sin(math.radians(200))), (24 + 10.2 * math.cos(math.radians(200)), 24 + 10.2 * math.sin(math.radians(200)))], 2.0, STRAW)
    g.ell(24, 24, 1.9, 1.9, fill=INK)
    return compose(MATCHA, [g])


def preview(t):
    g = Glyph()
    g.rr(10.5, 12, 27, 24, 3.6, outline=INK, width=2.4)
    g.poly([(13, 34), (13, 29.5), (19, 23.5), (24, 28.5), (28, 25), (35, 31), (35, 34)], INK)
    g.ell(30.2, 19.4, 3.2, 3.2, fill=HONEY)                            # the accent: the sun
    return compose(LAV, [g])


def activity(t):
    g = Glyph()
    x = 11.0
    for h in (10, 17, 13, 21, 8):
        g.rr(x, 36 - h, 4.4, h, 1.4, mix(TERM_BAR, TERM_TEXT, 0.18))
        x += 6.8
    g.line([(10, 28), (16, 28), (19.5, 17), (24.5, 31), (29, 20), (33, 20), (35, 24), (38.5, 24)], 3.0, MATCHA)   # the accent: the graph line
    return compose(TERM, [g], dark_shadow=True, edge=TERM_BAR if t is DARK else None)


def store(t):
    g = Glyph()
    g.rr(11.5, 18.5, 25, 20, 3.6, INK)
    g.d.arc([17.2 * U, 9.5 * U, 30.8 * U, 25.5 * U], 180, 360, fill=INK, width=int(2.8 * BOLD * U))
    g.poly([(24, 23), (25.4, 27), (29.4, 28.4), (25.4, 29.8), (24, 33.8), (22.6, 29.8), (18.6, 28.4), (22.6, 27)], hexc("FFFDF8"))   # the accent: a sparkle
    return compose(STRAW, [g])


def spaces(t):
    g = Glyph()
    for i, (x, y) in enumerate(((11, 12), (25.5, 12), (11, 26), (25.5, 26))):
        if i == 0:
            g.rr(x, y, 11.5, 10, 2.4, STRAW)                              # the accent: the current space
        else:
            g.rr(x, y, 11.5, 10, 2.4, None, outline=t["esp"], width=2.3)
    return compose(t["milk"], [g], border=t["sand"])


def file_document(t):
    g = Glyph()
    g.rr(13, 8.5, 22, 31, 3.2, None, outline=t["esp"], width=2.4)
    g.poly([(35, 8.5), (35, 17), (26.5, 8.5)], HONEY)                     # the accent: the folded corner
    for y, w in ((20, 13), (26, 13), (32, 8)):
        g.line([(17.5, y), (17.5 + w, y)], 2.3, t["cocoa"], False)
    return compose(t["milk"], [g], border=t["sand"])


def file_image(t):
    g = Glyph()
    g.rr(9.5, 12, 29, 24, 3.6, None, outline=t["esp"], width=2.4)
    g.poly([(12, 34), (12, 29.5), (18, 23.5), (23, 28.5), (27, 25), (36, 31), (36, 34)], t["esp"])
    g.ell(30.6, 19.2, 3.2, 3.2, fill=HONEY)                               # the accent: the sun
    return compose(t["milk"], [g], border=t["sand"])


def file_audio(t):
    g = Glyph()
    for cx, cy in ((17.6, 33.6), (31.2, 31.0)):
        g.rot_ell(cx, cy, 5.2, 3.9, 22, t["esp"])
    g.rr(20.4, 14.4, 2.6, 19.6, 0.9, t["esp"])
    g.rr(34, 11.8, 2.6, 19.6, 0.9, t["esp"])
    g.poly([(20.4, 14.4), (36.6, 11.2), (36.6, 17.0), (20.4, 20.2)], STRAW)   # the accent: the beam
    return compose(t["milk"], [g], border=t["sand"])


def file_archive(t):
    g = Glyph()
    g.rr(10.5, 15.5, 27, 22, 3.2, None, outline=t["esp"], width=2.4)
    g.rr(9, 10.5, 30, 7.5, 2.4, t["esp"])
    for k in range(4):
        g.rr(22.8, 20.5 + k * 4.2, 2.6, 2.6, 0.6, HONEY)                  # the accent: the zip
    return compose(t["milk"], [g], border=t["sand"])


def file_package(t):
    g = Glyph()
    g.rr(10.5, 14.5, 27, 23, 3.4, None, outline=t["esp"], width=2.4)
    g.line([(10.5, 21.5), (37.5, 21.5)], 2.2, t["esp"], False)
    g.rr(21, 14.5, 6, 23, 0.6, STRAW)                                     # the accent: the tape
    return compose(t["milk"], [g], border=t["sand"])


ICONS = [("files", files), ("terminal", terminal), ("browser", browser), ("notes", notes), ("paint", paint), ("music", music), ("settings", settings),
         ("text-editor", text_editor), ("calendar", calendar), ("clock", clock), ("preview", preview), ("activity", activity), ("store", store), ("spaces", spaces)]
FILE_ICONS = [("file-folder", files), ("file-document", file_document), ("file-image", file_image), ("file-audio", file_audio), ("file-archive", file_archive),
              ("file-package", file_package)]
FILE_SIZES = [56, 36]
SIZES = [512, 96, 48, 40, 36, 28, 20, 16]


BOLD_FOR = {56: 1.0, 512: 1.0, 96: 1.0, 48: 1.0, 40: 1.0, 36: 1.0, 28: 1.25, 20: 1.55, 16: 1.8}


def render_all(out):
    """Every icon at every size, light and dark. The sizes of 28 and under are redrawn with heavier strokes rather than scaled down."""
    global BOLD
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    for name, fn in ICONS:
        for theme, tok in (("", LIGHT), ("-dark", DARK)):
            cache = {}
            for s in SIZES:
                BOLD = BOLD_FOR[s]
                if BOLD not in cache:
                    cache[BOLD] = fn(tok)
                cache[BOLD].resize((s, s), Image.Resampling.LANCZOS).save(out / f"{name}-{s}{theme}.png")
    for name, fn in FILE_ICONS:
        for theme, tok in (("", LIGHT), ("-dark", DARK)):
            BOLD = 1.0
            m = fn(tok)
            for s in FILE_SIZES:
                m.resize((s, s), Image.Resampling.LANCZOS).save(out / f"{name}-{s}{theme}.png")
    BOLD = 1.0


if __name__ == "__main__":
    if "--border" in sys.argv:
        BAKE_BORDER = True
        sys.argv.remove("--border")
    render_all(sys.argv[1])
    print("ok")
