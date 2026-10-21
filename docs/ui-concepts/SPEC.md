# milkOS 1.0 UI spec

For the engineer building the compositor and shell. Generated from `v1.mjs` (the mockup uses the same numbers), so if you change a value there, regenerate. Mockup: `milkos-1.0.html`. Screen: 1024x768, 32-bit. Layout snaps to a 4px grid.

## Colour tokens (sRGB)
| Token | Hex | Use |
|---|---|---|
| milk | #FFFDF8 | window body, cards, launcher |
| cream | #FBF6EC | title bars, page background |
| oat | #EFE6D4 | sidebars, inactive fills |
| sand | #E2D5BD | hairlines, borders, tracks |
| latte | #C4AE8F | disabled text, inactive lights |
| cocoa | #7A6652 | secondary text |
| espresso | #3A2E28 | primary text, terminal background |
| straw | #E58C97 | accent (selection, close light, progress) |
| honey | #EBB650 | focus ring, minimise light, warnings |
| matcha | #9CBF9B | success, zoom light, good meters |
| blue | #7F93C9 | info, links, disk meter |
| lav | #B9A9D6 | wallpaper only |

Alpha tints used: straw at 0x44 (27%) for selection rows, straw at 0x55 (33%) for sidebar selection, honey at 0x55 for folder tiles, espresso for shadows (below).

## Type
- Unifont 8x16, 1x, espresso. Secondary text cocoa. Disabled latte.
- Bold: Same glyphs drawn twice, second copy at +1px x (faux bold). Nicer font later: real bold.
- Heading: Unifont at 2x (16x32), bold.
- Display: Unifont at 4x (32x64), bold. Clock only.
- No text smaller than 16px tall. Small caps labels are 8x16 uppercase in cocoa, not a smaller size.

## Corner radii (px)
- window: 12
- dock: 20
- launcher: 18
- card: 20
- toast: 16
- button: 8
- row: 12
- chip: 8
- tile: 24% of icon size
- light: circle d=12

## Glass (the cheap version)
- Keep two copies of the wallpaper: `W` (sharp) and `Wb` (box blur radius 24, 3 passes). Rebuild `Wb` only when the wallpaper changes.
- A glass panel = copy the `Wb` pixels under its rect, blend milk (#FFFDF8) at alpha 0.55 (launcher and control surfaces: 0.7), then a 1px inner border, white at alpha 0.75.
- Only panels directly over the wallpaper are glass (menu bar, dock, launcher, toasts, control centre, home cards). Windows are opaque.
- Live blur over windows is not supported. If a glass panel ever overlaps a window, draw it as plain milk at alpha 0.92.

## Shadows
Colour espresso (58,46,40). Stack the listed rounded rects (grown by `grow` px on every side, moved down `dy`, alpha `a`). No blur needed.
- Window (focused or not):
  1. grow 10px, move down 12px, alpha 0.05
  2. grow 5px, move down 8px, alpha 0.07
  3. grow 0px, move down 4px, alpha 0.12
- Floating panels (dock, toast, launcher, control centre, cards):
  1. grow 6px, move down 6px, alpha 0.05
  2. grow 3px, move down 4px, alpha 0.07

## Menu bar
Height 28. Glass, fill alpha 0.55, 1px white bottom edge. Left: logo drop 13x13 at x12, y8, then app name (bold) at x38, menu items separated by 24px (3 spaces of 8px). Right: status text right-aligned, 12px from edge. Text top at y6.

## Dock
Height 64, radius 20, glass. Padding 8, icons 48x48, gap 12. Width = 16 + n*48 + (n-1)*12. Centred horizontally, 12px above the bottom edge. Running app: 4x4 dot in cocoa, 4px above the dock's bottom edge, centred under the icon. Icon tile radius is 24% of its size.

## Window chrome
- Radius 12, 1px sand border (terminal: 1px #4A3C34), opaque body (milk; terminal espresso).
- Title bar height 32, fill cream (terminal #4A3C34), 1px sand line under it. Title text bold, horizontally centred in the bar, top at y8.
- Traffic lights: 12px circles, 8px gap, first light's top-left at (14,10) inside the window (border included). Order: close straw, minimise honey, zoom matcha. Unfocused window: all three latte, title text cocoa.
- Minimum size 240x160. Body padding 16. Files sidebar width 160 (oat, 1px sand right edge). Status bar height 24 (cream, 1px sand top edge).

## Launcher (Skim)
Width 640, centred, top at y140. Input row height 56, radius 18, glass alpha 0.7, query at 2x text, leading icon 40. Results panel 12px below, same radius. Result row height 48, radius 12, selected row straw at 27%, icon 36. Max 6 rows. Dims the screen behind with espresso at alpha 0.25.

## Toast / notification
264x64, radius 16, glass, 12px from the right edge, 40px from the top. Icon 40x40 at (12,12), title bold at (64,12), subtitle cocoa at (64,32).

## Notification drawer (side drawer)
Chosen look (Andrew, 2026-10-21). Panel 316 wide, 12px from the right edge, from y36 down to 12px above the bottom edge. It is drawn above the dock, so it covers the dock's right end while open. Radius 18, glass at alpha 0.78, floating shadow. Inner padding 12.
- Header (78px): "Notifications" at the top left, "Clear all" right-aligned in cocoa, then filter chips (All, Mentions, Today), 26px high, fully rounded, selected chip espresso with cream text.
- Group label: 8x16 uppercase cocoa ("MESSAGES"), count right-aligned. Groups collapse and expand.
- Card: width drawer minus 24, height 64 (compact 56), radius 14, milk at 82% with a 1px white edge. App icon 36x36 at (10,10); title bold at (56,8); time right-aligned in cocoa at the same line; one line of body in cocoa at (56,28), cut with "~" when too long. Selected card: straw at 13% fill and a 1px straw border.
- Today card: label, then up to three events (time bold, title). Do Not Disturb row: 44px high at the bottom with a switch (44x24, straw when on).
- Opening: click the menu bar bell, or Ctrl-N. Closing: Esc, or click outside. Snap open (no animation required).
- Keys: Up/Down select, Return opens the source app or window, Backspace dismisses the selected card, Left/Right collapse or expand a group, Tab cycles the filter chips.
- Arrival: a toast (above) shows for 5 seconds, pauses while hovered, and clicking it opens the drawer on that item. If the drawer is open, the new card appears at the top of its group instead.
- Badges: the menu bar bell shows the total unread count; dock icons show their own app's count (straw circle, 18px, milk 2px ring). Do Not Disturb holds toasts and sounds but still collects cards, and shows a moon on the bell.

## Control centre
288x292, radius 20, glass, 12px from right, 36px from top. Tiles/sliders 56 high, radius 16, white at 70%. Slider track 8 high, fill espresso.

## Home (Today) cards
Radius 20, padding 16, gap 16, 3-column grid inside 48px side margins. Labels are 8x16 uppercase cocoa.

## Spaces (overview)
Dim the wallpaper with espresso at 0.35. Space strip at y40: thumbnails 128x88, gap 12, radius 10; the current space has a 4px honey border. Window cells are each window's offscreen buffer scaled ≈0.29 (scaled blit), with the title under it in milk. Focused window: 4px honey border.

## Wallpaper
Dawn pasture: vertical gradient #FBE3C8 → #F3C3B8 → #C9B3D9, honey sun disc with halo, three overlapping matcha hill circles, tiny cow. Stored sharp (W) and pre-blurred (Wb = box blur r24 x3 passes, recomputed on change).

## What the mockup does not show
Hover/pressed states (lighten fills 4% / darken 4%), animation (none required for 1.0; Spaces can snap), cursor, lock screen, text selection colour (straw at 27%). Ask Mary if one of these blocks you.
