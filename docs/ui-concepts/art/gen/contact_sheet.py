#!/usr/bin/env python3
# Host-only: needs Pillow (and numpy for wallpapers.py) to regenerate the art. Not needed to build or run milkOS.
"""The contact sheet for the art hand-off: every icon at 48 and 16 in light and dark, a dock at the real size, the three wallpapers and the two splashes.

    python3 contact_sheet.py <dir with the icons, wallpapers and splash>
"""
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

D = Path(sys.argv[1])
NAMES = ["files", "terminal", "browser", "notes", "paint", "music", "settings", "text-editor", "calendar", "clock", "preview", "activity", "store", "spaces"]
try:
    FONT = ImageFont.load_default(size=13)
except TypeError:
    FONT = ImageFont.load_default()
CREAM, DARKBG, COCOA, LIGHT_TXT = (251, 246, 236), (46, 37, 32), (107, 88, 70), (190, 168, 144)


def icon(n, s, dark):
    return Image.open(D / "icons" / f"{n}-{s}{'-dark' if dark else ''}.png").convert("RGBA")


def icon_f(n, s, dark):
    return Image.open(D / "icons" / f"{n}-{s}{'-dark' if dark else ''}.png").convert("RGBA")


def mixc(a, b, t):
    return tuple(int(a[i] * (1 - t) + b[i] * t) for i in range(3))


W = 1320
rows = []
# the table of icons: light half on cream, dark half on dark cream, each icon as 48 and 16
sheet = Image.new("RGB", (W, 1200), CREAM)
d = ImageDraw.Draw(sheet)
d.text((16, 10), "icons: 48 and 16 px, light theme (left) and dark theme (right)", fill=COCOA, font=FONT)
for half, dark in enumerate((False, True)):
    x0 = 12 + half * (W // 2)
    bg = DARKBG if dark else CREAM
    d.rectangle([x0 - 8, 30, x0 + W // 2 - 20, 30 + 7 * 80 + 10], fill=bg)
    for i, n in enumerate(NAMES):
        col, row = i % 2, i // 2
        x, y = x0 + col * 320, 40 + row * 80
        t = Image.new("RGBA", (300, 72), bg + (255,))
        t.alpha_composite(icon(n, 48, dark), (8, 8))
        t.alpha_composite(icon(n, 16, dark), (66, 24))
        td = ImageDraw.Draw(t)
        td.text((92, 26), n, fill=LIGHT_TXT if dark else COCOA, font=FONT)
        sheet.paste(t.convert("RGB"), (x, y))
y = 30 + 7 * 80 + 24
# a dock at the real size, over each wallpaper's bottom strip
for k, (wp, dark) in enumerate((("wallpaper-dawn", False), ("wallpaper-night", True))):
    bg = Image.open(D / "wallpapers" / f"{wp}-1024x768.png").convert("RGB")
    strip = bg.crop((0, 668, 1024, 768)).resize((640, 62)) if False else bg.crop((0, 668, 1024, 768))
    glass = (255, 253, 248) if not dark else (46, 37, 32)
    sd = Image.new("RGBA", strip.size, (0, 0, 0, 0))
    ImageDraw.Draw(sd).rounded_rectangle([210, 24, 814, 88], radius=20, fill=glass + (int(255 * 0.6),), outline=(255, 255, 255, 190))
    base = strip.convert("RGBA")
    base.alpha_composite(sd)
    for i, n in enumerate(NAMES):
        base.alpha_composite(icon(n, 48, dark), (210 + 8 + i * 60 + 4, 32))
    sheet.paste(base.convert("RGB"), (k * (1024 + 8) if False else (k * 660, y)) if False else (16 + k * (W // 2 - 4), y))
    # crop the strip to half the sheet width
    break
sheet2 = sheet
# the strips, side by side, scaled to half width each
strips = []
for wp, dark in (("wallpaper-dawn", False), ("wallpaper-night", True)):
    bg = Image.open(D / "wallpapers" / f"{wp}-1024x768.png").convert("RGB").crop((0, 650, 1024, 768)).convert("RGBA")
    glass = (255, 253, 248) if not dark else (46, 37, 32)
    sd = Image.new("RGBA", bg.size, (0, 0, 0, 0))
    dock_apps = ["files", "terminal", "browser", "notes", "paint", "music", "calendar", "clock", "store", "settings"]
    dw = 16 + len(dock_apps) * 48 + (len(dock_apps) - 1) * 12
    dx = (1024 - dw) // 2
    ImageDraw.Draw(sd).rounded_rectangle([dx, 42, dx + dw, 106], radius=20, fill=glass + (int(255 * 0.6),), outline=(255, 255, 255, 190))
    bg.alpha_composite(sd)
    for i, n in enumerate(dock_apps):
        bg.alpha_composite(icon(n, 48, dark), (dx + 8 + i * 60, 50))
    strips.append(bg.convert("RGB").resize((W // 2 - 20, int(118 * (W // 2 - 20) / 1024))))
final = Image.new("RGB", (W, 1500), CREAM)
final.paste(sheet.crop((0, 0, W, 30 + 7 * 80 + 14)), (0, 0))
yy = 30 + 7 * 80 + 22
ImageDraw.Draw(final).text((16, yy), "the dock at 48 px over the two wallpapers", fill=COCOA, font=FONT)
yy += 20
for k, s in enumerate(strips):
    final.paste(s, (10 + k * (W // 2), yy))
yy += strips[0].height + 14
ImageDraw.Draw(final).text((16, yy), "wallpapers (dawn, night, oat) and splashes (light, night), 1024 x 768 shown at a quarter", fill=COCOA, font=FONT)
yy += 20
ImageDraw.Draw(final).text((16, yy - 2), "Files file-type icons at 56 and 36 px: folder, document, image, audio, archive, package (light, then dark)", fill=COCOA, font=FONT)
yy += 18
for dk in (False, True):
    bg = DARKBG if dk else CREAM
    final.paste(Image.new("RGB", (W // 2 - 20, 72), bg), (10 + int(dk) * (W // 2), yy))
    for i, n in enumerate(["file-folder", "file-document", "file-image", "file-audio", "file-archive", "file-package"]):
        t = Image.new("RGBA", (96, 72), (0, 0, 0, 0))
        t.alpha_composite(icon_f(n, 56, dk), (0, 8))
        t.alpha_composite(icon_f(n, 36, dk), (60, 26))
        final.paste(t, (16 + int(dk) * (W // 2) + i * 104, yy), t)
yy += 84
names = ["wallpapers/wallpaper-dawn-1024x768.png", "wallpapers/wallpaper-night-1024x768.png", "wallpapers/wallpaper-oat-1024x768.png", "splash/splash-1024x768.png", "splash/splash-night-1024x768.png"]
x = 12
for n in names:
    im = Image.open(D / n).convert("RGB").resize((256, 192), Image.Resampling.LANCZOS)
    final.paste(im, (x, yy))
    ImageDraw.Draw(final).rectangle([x - 1, yy - 1, x + 256, yy + 192], outline=(226, 213, 189))
    x += 264
yy += 206
final = final.crop((0, 0, W, yy))
final.save(D / "contact-sheet.png")
print("ok", final.size)
