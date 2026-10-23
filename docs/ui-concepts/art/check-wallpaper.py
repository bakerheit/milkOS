#!/usr/bin/env python3
"""Check a wallpaper against the glass-panel contrast rule in docs/ui-concepts/ART.md.

Glass = milk at 55% over the blurred wallpaper (dark theme: dark milk). Primary text on it must be >= 7:1
in the wallpaper's own theme, over every region where a glass panel can sit.

    python3 docs/ui-concepts/art/check-wallpaper.py wallpaper-dawn.png
    python3 docs/ui-concepts/art/check-wallpaper.py wallpaper-night.png --dark
    python3 docs/ui-concepts/art/check-wallpaper.py art/wallpapers-photos/dusk-pasture-1024x768.jpg --alpha 0.75

Needs Pillow. Prints one line per region and exits 1 if any region fails.
"""
import sys
from PIL import Image, ImageFilter

LIGHT = {"glass": (0xFF, 0xFD, 0xF8), "text": (0x3A, 0x2E, 0x28)}
DARK = {"glass": (0x2E, 0x25, 0x20), "text": (0xF4, 0xEB, 0xE0)}
ALPHA = 0.55          # glass fill alpha (SPEC); --alpha N overrides it (photo wallpapers use 0.75)
MIN_RATIO = 7.0
BLUR_RADIUS = 24      # the pre-blurred wallpaper uses a box blur of radius 24
REGIONS = {           # name: (x, y, w, h) on a 1024x768 screen (SPEC sizes)
    "menu bar": (0, 0, 1024, 28),
    "toast": (748, 40, 264, 64),
    "notification drawer": (696, 36, 316, 720),
    "control centre": (724, 36, 288, 292),
    "dock": (210, 692, 604, 64),
    "launcher": (192, 140, 640, 300),
    "today cards": (48, 56, 928, 560),
    "files sidebar and cards": (96, 64, 600, 400),
}


def lin(v):
    v /= 255
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def lum(c):
    return 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])


def ratio(a, b):
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


def main():
    global ALPHA
    argv = sys.argv[1:]
    if "--alpha" in argv:
        i = argv.index("--alpha")
        ALPHA = float(argv[i + 1])
        del argv[i:i + 2]
    args = [a for a in argv if not a.startswith("--")]
    if not args:
        print(__doc__)
        return 2
    theme = DARK if "--dark" in argv else LIGHT
    img = Image.open(args[0]).convert("RGB")
    if img.size != (1024, 768):
        print(f"warning: expected 1024x768, got {img.size[0]}x{img.size[1]}")
        img = img.resize((1024, 768))
    blurred = img.filter(ImageFilter.BoxBlur(BLUR_RADIUS))
    failed = False
    for name, (x, y, w, h) in REGIONS.items():
        region = blurred.crop((x, y, x + w, y + h))
        worst = 99.0
        # sample on a grid: the worst place is where the glass (and so the background) is darkest or lightest
        step = max(4, min(w, h) // 12)
        for yy in range(0, h, step):
            for xx in range(0, w, step):
                p = region.getpixel((xx, yy))
                bg = tuple(p[i] * (1 - ALPHA) + theme["glass"][i] * ALPHA for i in range(3))
                worst = min(worst, ratio(theme["text"], bg))
        ok = worst >= MIN_RATIO
        failed |= not ok
        print(f"{'PASS' if ok else 'FAIL'}  {name:26s} worst primary-text contrast {worst:5.2f}:1 (need {MIN_RATIO}:1)")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
