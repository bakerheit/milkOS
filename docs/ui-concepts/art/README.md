# milkOS look pass: icons, wallpapers, boot splash

Everything here is original work, drawn by the scripts in `gen/` (Python and Pillow only, no outside images, fonts or samples), and is licensed with the repository (MIT).
It follows `ART.md` and the tokens in `SPEC.md`. Open `contact-sheet.png` first.

## What is here
- `icons/<app>-<size>.png` and `icons/<app>-<size>-dark.png`, for **files, terminal, browser, notes, paint, music, settings, text-editor, calendar, clock, preview, activity**,
  at **512, 96, 48, 40, 36, 28, 20 and 16** px. RGBA with transparent corners (the tile radius is 24% of the size). The `-dark` file is identical to the light one for the
  accent tiles (honey, blue, straw, matcha, lavender: ink glyph in both themes) and for Terminal and Activity (the fixed Terminal body colour); it differs for the neutral
  tiles (notes, paint, text-editor, calendar, settings), mapped through the dark table of the spec (milk tile `#2E2520`, glyph `#F4EBE0`, latte tile `#8C7764`).
- `wallpapers/wallpaper-dawn|night|oat-1024x768.png`. Dawn pasture, Night pasture (moonrise at the bottom left, stars, a sleeping cow) and Oat.
- `splash/splash-1024x768.png` and `splash-night-1024x768.png`.
- `contact-sheet.png`: every icon at 48 and 16, light and dark, the dock at 48 px over two wallpapers, the wallpapers and the splashes.
- `gen/`: the scripts that make all of it (`icons.py`, `wallpapers.py`, `contact_sheet.py`). `python3 gen/icons.py out/`, `python3 gen/wallpapers.py out/` (this also writes
  2048x1536 versions), `python3 gen/contact_sheet.py <folder>`.

## The colour map (one tile colour per app)
Files honey; Terminal espresso; Browser blue; Notes milk with a honey band; Text Editor milk with cocoa lines and a straw caret; Paint milk with four colour dots;
Music straw; Calendar milk with a straw band and the date numeral; Clock matcha; Preview lavender; Settings latte; Activity Monitor espresso with a matcha graph line.

## Checks that were run
- **Wallpapers:** `art/check-wallpaper.py` prints PASS for all eight regions on dawn, oat and night (`--dark`); the tightest is the night drawer region at about 7.2:1 to 8.6:1,
  the rest are above 9:1. The night picture keeps its moon and cow away from the glass regions on purpose (a bright thing under dark glass cannot reach 7:1).
  The darkest value in Night is above `#1E1915`.
- **Icons:** glyph about 55 to 60% of the tile, strokes at least 2 units on the 48 grid, ink on accent tiles, at most one small accent, a faint top light and a soft shadow.
  The sizes of 28, 20 and 16 are drawn again with heavier strokes (and, at 20 and 16, fewer details: no globe meridian, no clock ticks, fewer note lines), not scaled down.
  Squint test: the contact sheet at half size still tells the twelve apart by tile colour and shape.

## Notes for whoever embeds them
- The light tiles (milk) carry the spec's 1px sand edge in the PNG so they do not vanish on a cream title bar. The shell's own drop shadow is not baked in.
- The splash has the drop, the wordmark and the bar's track (220 x 4); the bar's fill and the word "booting" are left to the kernel, which draws them live. The wordmark is
  drawn with round-capped strokes (own design, no font), so it may be swapped for the OS font if that suits better. The drop is the pointed teardrop of the current build.
- Not done, because not asked yet: Store and Spaces icons, the file-type icons for Files, and the optional fourth wallpaper.
