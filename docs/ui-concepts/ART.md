# milkOS art direction (icons, wallpapers, boot splash, sounds)

Owner: the UI lead. Used by whoever draws or synthesises the assets and by the engineers who embed them. Read with `SPEC.md` (tokens, dark theme, buttons). A visual reference is in `art-direction.html`. Wallpaper contrast is checked with `art/check-wallpaper.py`.

## 1. The feeling
**Whole milk: soft, calm, warm, a little playful.** Cream and oat for surfaces, one friendly pink (straw) for action, small honey, matcha, blue and lavender accents. The mascots are the milk drop and the cow. Things are rounded, flat and gently lit: a faint highlight on top, a soft shadow below, never a hard bevel. Nothing is loud, neon, pure black or pure white. When in doubt, quieter.

Three tests every asset must pass:
1. **Squint test:** at half size, blurred, it is still recognisable and still looks like milkOS.
2. **Neighbour test:** next to its siblings (the dock, a Files window, the Welcome tour) it looks like family: same corner radius, same light direction, same line weight.
3. **Both themes:** it works on the light and the dark ("Chocolate milk") theme.

## 2. Colour
Use only the SPEC tokens: milk #FFFDF8, cream #FBF6EC, oat #EFE6D4, sand #E2D5BD, latte #C4AE8F, cocoa #6B5846, espresso #3A2E28, ink #3A2E27, and the accents straw #E58C97, honey #EBB650, matcha #9CBF9B, blue #7F93C9, lavender #B9A9D6 (peach #F2B79B for wallpaper skies only). Pure #000 and #FFF are not used. Shadows are ink at 5 to 14% alpha. If a new colour is truly needed, ask for a token first.

## 3. Icons
**Language: a flat glyph on a rounded tile.** Tile corner radius is 24% of its size. Keep it. Enrich, do not replace.
- Glyph fills about 55 to 60% of the tile, centred, with a little optical correction. It never breaks out of the tile.
- One glyph per app, one small accent at most (a straw band, a matcha line, a caret).
- Strokes at least 2 px at 48 and 1 px at 24. Nothing finer: it must survive the kernel's scaling.
- Glyph colour is **ink #3A2E27 on every accent tile, in both themes**. On neutral tiles (milk, espresso, latte) the dark theme maps the tile and the glyph through the dark table (milk tile -> #2E2520, glyph -> #F4EBE0).
- Allowed finish: faint top highlight (white at 12 to 18%, upper third), soft shadow under the glyph (ink at 8%), nothing else. No gradients across the whole tile, no outlines, no text except Calendar's date numeral.
- **Sizes to deliver:** 512 master; 96, 48 (dock), 40 (launcher), 36 (drawer cards), 28 (taskbar), 20 and 16 (lists, menu bar). Hand-tune 16 to 28 rather than just scaling.

**Colour map (one tile colour per app, siblings differ at a glance):**
| App | Tile | Glyph and accent |
|---|---|---|
| Files | honey | folder, ink |
| Terminal | espresso | ">_" in cream |
| Activity Monitor | espresso | line graph with one matcha line (family: system tools) |
| Browser | blue | globe |
| Notes | milk | pad with a honey band across the top |
| Text Editor | milk | portrait page with a folded corner, cocoa outline and lines, a straw caret on the last line |
| Paint | milk | four colour dots (straw, honey, matcha, blue) |
| Music | straw | note |
| Calendar | milk | straw band at the top, date numeral in ink |
| Clock / Timer | matcha | clock face with two hands |
| Preview | lavender | picture frame with a sun and a hill |
| Settings | latte | gear |
| Store | straw | open box or "S" bag |
| Spaces / Overview | milk | four small windows |
| Today | milk | a honey sun rising over a cocoa horizon with five short rays (a scene, no text lines) |
No two neighbouring dock tiles share a colour. **The three neutral tiles with a glyph made of lines must not look alike:** Notes is a wide pad with a honey band and ink lines; Text Editor is a portrait outlined page; Today has no lines at all. (Review 2026-10-23: Notes, Today and Text Editor all read as "three lines on white" in the dock.) Folder and file-type icons (document, image, audio, archive, package) use the same tile language at 56 px in Files, with milk tiles and one accent.

## 4. Wallpapers
Set: **Dawn pasture** (default, light), **Night pasture** (dark, with moon, stars and a sleeping cow), **Oat** (plain), and an optional fourth (for example a dusk). All 1024x768, no text, no logos.
- Composition: a soft sky gradient, one sun or moon disc with a halo, two or three overlapping hills made from big circles, the cow on the lower right. Flat colour, no photographic noise.
- **Calm zones:** the top 28 px (menu bar), the top-right 316x720 band (notification drawer, toast, control centre), the bottom 76 px (dock) and the middle (Today cards, launcher) must stay low in detail. Put the sun and the cow where they are not under a glass panel for long.
- **Contrast rule:** glass is milk at 55% over the blurred wallpaper (dark theme: dark milk). Primary text on a glass panel over any part of the wallpaper must stay at least **7:1** in the theme the wallpaper belongs to. Check with `python3 docs/ui-concepts/art/check-wallpaper.py <wallpaper.png> [--dark]`; it must print PASS for every region.
- Dark wallpapers must not be pure black: lowest value about #1E1915.

### Photo wallpapers
Six CC0 photographs sit in `art/wallpapers-photos/` (sources, authors and licence in the README there): Belfast Sunset, Bell Park Dawn, Aarfontein Sunrise, Misty Dawn Field, Dusk Pasture, Cows and Trees. Rules:
- They are used **as shot**, no wash. The glass behind panels is **75% milk** (dark theme: 75% dark milk) instead of the illustrated wallpapers' 55%; at 75% every region of all six passes the 7:1 rule (`check-wallpaper.py --alpha 0.75`). The illustrated wallpapers keep 55%.
- They are extra choices beside Dawn pasture, Night pasture and Oat, not replacements. Night pasture stays the dark theme's default; a photo can be chosen in either theme.
- They are not baked into the kernel: raw RGB photos are about 2.3 MB each and do not run-length code. Load the chosen one at selection time (or ship them as files).
- A new photo needs a CC0 or public-domain licence, a record in that README, and no EXIF/GPS in the file.

## 5. Boot splash
Cream (#FBF6EC) or, for the night variant, dark cream (#382D27) at 1024x768. The milk drop (about 120 px tall, soft outline, a small highlight, a faint shadow ellipse), the wordmark "milkOS" below it with "OS" in blue, a 220x4 progress bar in straw on sand, and one small status word ("booting") in secondary text. Centre the group slightly above the middle. No animation beyond the bar. It must show within the first second and hand over to the desktop with a short fade (the existing 216 ms desktop-up is the budget).

## 6. Sounds (for the sound designer)
**The feeling:** a soft bell in a quiet kitchen. Warm, round, short. Original synthesis only (the repo is MIT; no samples). Reproducible: every sound comes from a script (`scripts/gen_sounds.py`), so there are no mystery binaries.
- Timbre: sine or very soft triangle with a gentle second and third harmonic, soft attack (5 to 15 ms), exponential decay. No harsh edges above about 3 kHz, no noise bursts except the shutter.
- Key and shape: C major pentatonic around G4 to C6. **Rising = good news, falling = a problem.** Two notes are plenty.
- Level: 48 kHz stereo 16-bit; peak at most -6 dBFS (no clipping); typical level about -22 dBFS RMS. No sound is louder than the boot chime.
- Length: UI sounds at most 0.5 s (notify 0.4 s, error 0.45 s); boot chime about 1 s; shutter 0.1 s; an alarm is a 2 s phrase that may loop. Respect Focus: when Focus is on, nothing plays except the alarm that the user set.
| Event | Intent | Shape |
|---|---|---|
| Boot chime | "hello, you are home" | G5 then C6, soft bell, about 1 s (exists) |
| Notify | "something for you" | A5 then E6, short (exists) |
| Error | "that did not work", never scolding | G4 then Eb4, soft, about 0.45 s (exists) |
| Screenshot | camera, polite | very short soft click (exists) |
| Milk Drop received | "arrived" | rising three-note arpeggio C6 E6 G6, 0.5 s |
| Restore / snapshot taken | "safe" | low warm two-note G4 C5, 0.4 s |
| Trash / delete | "gone" | soft downward slide, 0.3 s |
| Lock and unlock | latch | lock: falling C5 G4; unlock: rising G4 C5; 0.25 s |
| Focus on and off | gentle | off/on: a single soft note an octave apart, 0.3 s |
| Timer done | "your tea is ready" | three-note phrase repeated twice, 2 s, loopable |
| Calendar alert | polite nudge | two-note phrase, 1.2 s, repeats once if not dismissed |
| Volume tick | feedback | single soft tick, 0.08 s |
Distinct silhouettes: nobody should have to look to know which sound it was.

## 7. Hand-off
- Folder in the repo: `docs/ui-concepts/art/` for the references and notes; shipped assets go where the engineers say.
- File names: `<app>-<size>.png` and `<app>-<size>-dark.png`; wallpapers `wallpaper-<name>.png`; splash `splash.png` and `splash-night.png`; sounds `<event>.wav` (48 kHz stereo, 16-bit).
- Each delivery includes a **contact sheet** (all icons at 48 and 16, light and dark, side by side) and the checks above: the squint test, the contrast check for wallpapers, and `wav_check.py` for sounds. The UI lead reviews the contact sheet before engineers embed anything.
- Public repo rules: no names, tickets or personal paths in files, notes or file names; nothing downloaded without the project owner's yes.

## 8. Do not
Gradients that fight the flat look; text inside icons (except the Calendar numeral); pure black or white; saturated colours outside the tokens; glyphs that need more than 2 px strokes at 48; sounds with a hard attack, a high shriek or a long tail; anything that says "default operating system".
