# milkOS look pass: icons, wallpapers, boot splash

Everything here is original work, drawn by the scripts in `gen/` (Python and Pillow only, no outside images, fonts or samples), and is licensed with the repository (MIT).
It follows `ART.md` and the tokens in `SPEC.md`. Open `contact-sheet.png` first.

## What is here
- `icons/<app>-<size>.png` and `icons/<app>-<size>-dark.png`, for **files, terminal, browser, notes, paint, music, settings, text-editor, calendar, clock, preview, activity,
  store, spaces**, at **512, 96, 48, 40, 36, 28, 20 and 16** px. RGBA with transparent corners (the tile radius is 24% of the size). The `-dark` file is identical to the
  light one for the accent tiles (honey, blue, straw, matcha, lavender: ink glyph in both themes); it differs for the neutral tiles (notes, paint, text-editor, calendar,
  settings, spaces), mapped through the dark table of the spec (milk tile `#2E2520`, glyph `#F4EBE0`, latte tile `#8C7764`). Terminal and Activity keep the fixed Terminal
  body colour in both themes, and their `-dark` files add a 1px `#4A3C34` edge so the tile does not vanish on dark glass.
- `icons/file-<kind>-<size>[-dark].png` for the Files file types (**folder, document, image, audio, archive, package**) at **56 and 36** px: the folder on a honey tile, the
  others on milk tiles with one accent each.
- `wallpapers/wallpaper-dawn|night|oat-1024x768.png`. Dawn pasture, Night pasture (moonrise at the bottom left, stars, a sleeping cow) and plain Oat.
- `splash/splash-1024x768.png` and `splash-night-1024x768.png`: the background, the drop and the bar's track (220 x 4). The wordmark area (about 384 x 64, centred under the
  drop), the bar's fill and the caption line are left clear for the kernel to draw in the OS font.
- `contact-sheet.png`: every icon at 48 and 16, light and dark, the dock at 48 px over two wallpapers, the file types, the wallpapers and the splashes.
- `gen/`: the scripts that make all of it (`icons.py`, `wallpapers.py`, `contact_sheet.py`; host-only, they need Pillow and, for the wallpapers, numpy, and are not needed to
  build milkOS). `python3 gen/icons.py out/`, `python3 gen/wallpapers.py out/` (this also writes 2048x1536 versions), `python3 gen/contact_sheet.py <folder>`.
  Flags: `icons.py --border` bakes the spec's 1px sand edge into the light tiles (off by default: the shell draws it); `wallpapers.py --wordmark` draws a round-stroke
  "milkOS" on the splash (off by default: the kernel draws it).

## The colour map (one tile colour per app)
Files honey; Terminal espresso; Browser blue; Notes milk with a honey band; Text Editor milk with cocoa lines and a straw caret; Paint milk with four colour dots;
Music straw; Calendar milk with a straw band and the date numeral; Clock matcha; Preview lavender; Settings latte; Activity Monitor espresso with a matcha graph line;
Store straw with an ink bag and a milk sparkle; Spaces milk with four small windows (the current one straw).

## Checks that were run
- **Wallpapers:** `art/check-wallpaper.py` prints PASS for all eight regions on dawn, oat and night (`--dark`). Night is the tight one (its drawer region is about 7.7:1), which
  is why its moon and cow sit where they do: a bright thing under dark glass cannot reach 7:1, so they must not be moved into a glass region. The darkest value in Night is
  above `#1E1915`. Oat is plain: a slow gradient, a hair of grain against banding and a highlight at 3%.
- **Icons:** glyph about 55 to 60% of the tile, strokes at least 2 units on the 48 grid, ink on accent tiles, at most one small accent, a faint top light and a soft shadow.
  The sizes of 28, 20 and 16 are drawn again with heavier strokes (and, at 20 and 16, fewer details: no globe meridian, no clock ticks, fewer note lines, a six-tooth gear), not
  scaled down. Squint test: the contact sheet at half size still tells the icons apart by tile colour and shape. At 16 px Settings shows its teeth and Clock a hand.

## Notes for whoever embeds them
- The milk tiles have no edge in the PNG; the shell draws the 1px sand edge as the spec's icon code does. The shell's own drop shadow is not baked in either.
- Not done: the optional fourth wallpaper.
