# milkOS 1.0 UI spec

For the engineer building the compositor and shell. Generated from `v1.mjs` (the mockup uses the same numbers), so if you change a value there, regenerate. Mockup: `milkos-1.0.html`. App specs (Activity Monitor, Text Editor, Calendar, Clock/Timer, Preview): `SPEC-APPS.md`. Art direction: `ART.md`. Screen: 1024x768, 32-bit. Layout snaps to a 4px grid.

## Colour tokens (sRGB)
| Token | Hex | Use |
|---|---|---|
| milk | #FFFDF8 | window body, cards, launcher |
| cream | #FBF6EC | title bars, page background |
| oat | #EFE6D4 | sidebars, inactive fills |
| sand | #E2D5BD | hairlines, borders, tracks |
| latte | #C4AE8F | disabled text, inactive lights |
| cocoa | #6B5846 | secondary text (was #7A6652: 4.40:1 on the sidebar, now 5.45:1) |
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

## Tabs (Terminal tabs and Tabs-everywhere window tabs share this)
One look for every tab strip, so a Terminal's tabs and a Files window that has absorbed a Terminal look the same. Added 2026-10-21 at the engineer's request.
- Strip: a window with two or more tabs uses a 38px bar in place of the 32px title bar. Same fill, 1px sand line under it. The traffic lights keep their x (14) and sit at y12, centred in the taller bar.
- Tab: height 32, top y6 inside the strip (so it touches the line under the strip), width between 96 and 160 (tabs share the strip equally, never narrower than 96), 6px between tabs. The first tab starts at x84. Corner radius 10px on the top corners only, square at the bottom. Padding 12px left, 8px right.
- Active tab: fill is the window body colour (milk), 1px sand border on the top, left and right but not the bottom, so it joins the content. Title in primary text, bold.
- Inactive tab: no fill, no border, title in secondary text. Hover: oat at 60% fill.
- Title: one line, cut with "~" to fit (about 13 characters in the default 144px tab). Terminal tabs read "1 msh", "2 msh".
- Close x: the letter x in the secondary colour of the tab it sits on (a light tab: secondary text; a Terminal-coloured tab: #BEA890 in any window, so a Terminal tab merged into a light window must not use the light cocoa, which is only 1.94:1 on the Terminal fill), right-aligned with 8px right padding, hit target 20x20. Shown on every tab at rest (not only on hover). Hover: a 20px circle in sand appears behind it and the x turns primary text. Pressing it closes the tab; closing the last tab closes the window.
- New tab (+): 28x24, radius 8, oat fill, 4px after the last tab, the "+" centred.
- Terminal tabs: same geometry with Terminal colours. Bar and border #4A3C34, active tab fill is the Terminal body #3A2E29 with Terminal text #FBF6ED, inactive title and close x in the dark cocoa #BEA890 (4.63:1 on the bar; the lighter latte would be only 2.48:1), hover circle #55463C. These colours are the same in the light and dark themes.
- Dragging a tab or a window onto a strip: the dragged item is lifted with the floating shadow. The drop target on the strip is a rounded rect where the new tab would go, straw at 20% fill, 2px dashed straw border, radius 10, with "Drop to add as a tab" in primary text, bold (straw text on the straw tint is only 1.96:1; the dashed straw border carries the colour), and at least 176px wide so the label fits. The dragged window ghost is at 92% opacity with a 2px straw border. Dragging a tab out of its strip makes it a window again.
- Keys: Cmd-T new tab, Cmd-W close tab, Ctrl-Tab and Ctrl-Shift-Tab next and previous, Cmd-1 to Cmd-9 jump. The first build kept the existing Terminal keys Cmd-T and Cmd-W, so the spec now matches it. Ctrl may be accepted as an alias where it is free.
- Notifications attached to tabs (a dot on the strip) are parked for later and not part of this spec.

## Launcher (Skim)
Width 640, centred, top at y140. Input row height 56, radius 18, glass alpha 0.7, query at 2x text, leading icon 40. Results panel 12px below, same radius. Result row height 48, radius 12, selected row straw at 27%, icon 36. Max 6 rows. Dims the screen behind with espresso at alpha 0.25.

## Toast / notification
264x64, radius 16, glass, 12px from the right edge, 40px from the top. Icon 40x40 at (12,12), title bold at (64,12), subtitle cocoa at (64,32).

## Notification drawer (side drawer)
Chosen look (Andrew, 2026-10-21). Panel 316 wide, 12px from the right edge, from y36 down to 12px above the bottom edge. It is drawn above the dock, so it covers the dock's right end while open. Radius 18, glass at alpha 0.78, floating shadow. Inner padding 12.
- Header (78px): "Notifications" at the top left, "Clear all" right-aligned in cocoa, then filter chips (All, Mentions, Today), 26px high, fully rounded, selected chip espresso with cream text.
- Group label: 8x16 uppercase cocoa ("MESSAGES"), count right-aligned. Groups collapse and expand.
- Card: width drawer minus 24, height 64 (compact 56), radius 14, milk at 82% with a 1px white edge. App icon 36x36 at (10,10); title bold at (56,8); time right-aligned in cocoa at the same line; one line of body in cocoa at (56,28), cut with "~" when too long (the font only covers U+0020 to U+00FF, so there is no ellipsis character). Selected card: straw at 13% fill and a 1px straw border.
- Today card: label, then up to three events (time bold, title). Focus row (the label is "Focus", not "Do Not Disturb": it is the same single setting as the Control centre Focus tile, with the sub-line "On until 12:00" while on): 44px high at the bottom with a switch (44x24, straw when on).
- Opening: click the menu bar bell, or Ctrl-N. Closing: Esc, or click outside. Snap open (no animation required).
- Keys: Up/Down select, Return opens the source app or window, Backspace dismisses the selected card, Left/Right collapse or expand a group, Tab cycles the filter chips.
- Arrival: a toast (above) shows for 5 seconds, pauses while hovered, and clicking it opens the drawer on that item. If the drawer is open, the new card appears at the top of its group instead.
- Badges: the menu bar bell shows the total unread count; dock icons show their own app's count (straw circle, 18px, milk 2px ring). Focus holds toasts and sounds but still collects cards, and shows a moon on the bell.

## Control centre
288x292, radius 20, glass, 12px from right, 36px from top. Tiles/sliders 56 high, radius 16, white at 70%. Slider track 8 high, fill espresso.

## Home (Today) cards
Radius 20, padding 16, gap 16, 3-column grid inside 48px side margins. Labels are 8x16 uppercase cocoa.

## Spaces (overview)
Dim the wallpaper with espresso at 0.35. Space strip at y40: thumbnails 128x88, gap 12, radius 10; the current space has a 4px honey border. Window cells are each window's offscreen buffer scaled ≈0.29 (scaled blit), with the title under it in milk. Focused window: 4px honey border.

## Wallpaper
Dawn pasture: vertical gradient #FBE3C8 → #F3C3B8 → #C9B3D9, honey sun disc with halo, three overlapping matcha hill circles, tiny cow. Stored sharp (W) and pre-blurred (Wb = box blur r24 x3 passes, recomputed on change).

## Dark theme ("Chocolate milk")
Values from kernel/src/wm/theme.rs tc() as given by the engineer, 2026-10-21. Every light neutral token maps to a dark one. The accents (straw, honey, matcha, blue, lav) are unchanged in both themes.
| Token | Light | Dark | Use |
|---|---|---|---|
| milk | #FFFDF8 | #2E2520 | window body, cards, launcher |
| cream | #FBF6EC | #382D27 | title bars, page background |
| oat | #EFE6D4 | #42352D | sidebars, inactive fills |
| sand | #E2D5BD | #55463C | hairlines, borders, tracks |
| latte | #C4AE8F | #8C7764 | disabled text, inactive lights |
| cocoa | #6B5846 | #BEA890 | secondary text (was #7A6652: 4.40:1 on the sidebar, now 5.45:1) |
| espresso | #3A2E28 | #F4EBE0 | primary text, terminal background |
| white | #FFFFFF | #7A6658 | glass edges, hover lightening |

Fixed in both themes (deliberately one step off the tokens above so the dark map skips them):
- Terminal body: #3A2E29
- Terminal text: #FBF6ED
- Ink (shadows, dims, glyphs on honey/straw/blue tiles): #3A2E27
- Terminal bar and border: #4A3C34

- Glass in dark: re-tinted from the dark milk, alphas unchanged (panels 0.55, launcher and control surfaces 0.7, border 0.75).
- Wallpaper: "Night pasture" (moon, stars, sleeping cow). Settings offers Auto, Dawn, Night and Oat.

### Contrast, measured from these exact values (WCAG AA: 4.5:1 for text, 3:1 for icons)
Light theme:
| Pair | Colours | Ratio | AA |
|---|---|---|---|
| Primary text on window body | #3A2E28 on #FFFDF8 | 12.90:1 | pass |
| Primary text on title bar | #3A2E28 on #FBF6EC | 12.17:1 | pass |
| Primary text on sidebar | #3A2E28 on #EFE6D4 | 10.58:1 | pass |
| Secondary text on window body | #6B5846 on #FFFDF8 | 6.64:1 | pass |
| Secondary text on title bar | #6B5846 on #FBF6EC | 6.27:1 | pass |
| Secondary text on sidebar | #6B5846 on #EFE6D4 | 5.45:1 | pass |
| Primary text on a selected sidebar row | #3A2E28 on #eccec4 | 8.86:1 | pass |
| Secondary text on a selected sidebar row (not allowed) | #6B5846 on #eccec4 | 4.56:1 | pass |
| Tab close x, resting (secondary colour) on the title bar | #6B5846 on #FBF6EC | 6.27:1 | pass |
| Tab close x on hover (primary) on its hover circle (sand) | #3A2E28 on #E2D5BD | 9.05:1 | pass |
| Disabled text on window body (exempt from AA) | #C4AE8F on #FFFDF8 | 2.11:1 | n/a |

Dark theme:
| Pair | Colours | Ratio | AA |
|---|---|---|---|
| Primary text on window body | #F4EBE0 on #2E2520 | 12.70:1 | pass |
| Primary text on title bar | #F4EBE0 on #382D27 | 11.32:1 | pass |
| Primary text on sidebar | #F4EBE0 on #42352D | 10.01:1 | pass |
| Secondary text on window body | #BEA890 on #2E2520 | 6.56:1 | pass |
| Secondary text on title bar | #BEA890 on #382D27 | 5.85:1 | pass |
| Secondary text on sidebar | #BEA890 on #42352D | 5.17:1 | pass |
| Primary text on a selected sidebar row | #F4EBE0 on #6e4c4a | 6.37:1 | pass |
| Secondary text on a selected sidebar row (not allowed) | #BEA890 on #6e4c4a | 3.29:1 | FAILS 4.5:1 |
| Tab close x, resting (secondary colour) on the title bar | #BEA890 on #382D27 | 5.85:1 | pass |
| Tab close x on hover (primary) on its hover circle (sand) | #F4EBE0 on #55463C | 7.65:1 | pass |
| Disabled text on window body (exempt from AA) | #8C7764 on #2E2520 | 3.52:1 | n/a |

Terminal (same in both themes):
| Pair | Colours | Ratio | AA |
|---|---|---|---|
| Terminal text on Terminal body | #FBF6ED on #3A2E29 | 12.17:1 | pass |
| Terminal tab, inactive (dark cocoa) on Terminal bar | #BEA890 on #4A3C34 | 4.63:1 | pass |
| Terminal tab, active text on Terminal body | #FBF6ED on #3A2E29 | 12.17:1 | pass |

Rules that come out of the numbers:
- **Secondary text never goes on a selected row.** A selected row is straw at 27% over the surface. Primary text on it is fine in both themes; secondary text on it is only about 3.3:1 in dark, so use primary there.
- **cocoa changed (light):** #7A6652 was 4.40:1 on the sidebar, just under 4.5. It is now #6B5846 (5.45:1 on the sidebar). The dark cocoa #BEA890 is unchanged. Anything hard-coded to #7A6652 should move to the token.
- **Disabled text (latte) is exempt** from the 4.5:1 rule but should not carry meaning on its own. The tab close x uses cocoa, not latte, for that reason.
- **Dimming an inactive window must not drop its text below 4.5:1.** In the dark theme the shipped inactive Terminal text is #8C7764 on #3A2E29, 3.08:1 (the light theme uses #C4AE8F on the same body, 6.11:1). Use #BEA890 (5.74:1) for dimmed Terminal text, or dim the window with a veil instead of recolouring the text.
- **Hairlines** (sand) are decorative dividers, about 1.3:1 to 1.7:1 in dark. Where a border is the only thing showing a control's edge (inputs, buttons), add a fill or use a stronger edge.

## Buttons
Added 2026-10-21 after a contrast check of the shipped screens: filled buttons were cream text on straw, 2.42:1. The fix keeps the pink and darkens the text. The text colour is the fixed ink (#3A2E27), not espresso, because espresso turns light in the dark theme.
| Kind | Fill | Text | Contrast |
|---|---|---|---|
| Primary (filled) | straw #E58C97 | ink #3A2E27, bold | 5.33:1 |
| Primary, hover | #EA9AA4 | ink | 6.04:1 |
| Primary, pressed | #DE7D89 | ink | 4.63:1 |
| Secondary | oat, 1px sand border | primary text (espresso; #F4EBE0 in dark) | 10.58:1 light, 10.01:1 dark |
| Danger | #B5473B | #FFFDF8 (fixed, not the milk token) | 5.26:1 |
| Disabled | oat, 1px sand border | latte (exempt from 4.5:1, never the only signal) | 1.73:1 |
- Sizes: pill (inside cards, toasts, banners) height 24, fully rounded (radius 12), padding 14 each side. Dialog button (sheets, panels, Open/Save) height 28, radius 8, minimum width 88. Text is bold (the faux bold).
- Button row in a sheet: right-aligned, 8px apart, the primary on the far right, the cancel or secondary action to its left. A destructive primary uses the Danger kind.
- Focus ring: 2px honey, 2px outside the button. Pressed buttons move nothing.
- Where it applies: every filled straw pill (Welcome tour buttons, Restore this version, Share, Send), dialog buttons (Open, Cancel), and sheets. Straw is still used freely for fills without text on them (selection, progress, the traffic light).

## Files window: toolbar, sidebar and status bar
Proposal from the 2026-10 screen review. Today the window has only a sidebar, a grid and a status bar with an "up" pill squeezed into the corner.
- Toolbar: 40px high, directly under the title bar, cream, 1px sand line below. Left to right at 12px padding: back and forward (24px circles, oat; forward is latte when disabled), the path bar (24px high, radius 12, milk with 1px sand, "Home > Documents", each part clickable, replaces the old "up" pill), search (150px wide, same style), the Share button (a secondary pill; primary while a file is selected), and a view menu button (list or grid).
- Sidebar (168 wide): FAVOURITES (Home, Documents, Downloads, Desktop, Pictures, Music), NEARBY (Milk Drop devices found on the link: a dot, green when the device is receiving and latte when not; dropping a file on a name sends it), SHARED (the Mac folder). "milk" and "Milk" are not good labels (they differ by a capital): name them "This machine" and "Milk disk". Time Machine is not a device: give it a clock button in the toolbar (it opens the versions view) and keep the "Browse All Versions" menu item.
- Status bar: 24 high. Left to right: "1 of 4 selected, 1.0 MB", then "212 GB available" on the right. Path text uses secondary text, never latte (latte on cream is 1.99:1).
- Empty folder: a centred line in secondary text, "This folder is empty", and a faint dashed outline where dropped files would go.

## Menu bar status area
The right side of the menu bar today shows only the CPU meter and the clock, so there is nowhere to open the notification drawer from and no sign of Focus, network or sound. Right to left from the clock, 12px apart, each a 16px glyph with a 28px high hit target:
1. Clock (opens Today).
2. CPU meter (opens Activity).
3. Volume (opens the Control centre).
4. Network (opens the Control centre).
5. Focus moon, shown only while Focus is on.
6. Bell with the unread count (opens the notification drawer). This is the one that must exist before the drawer ships.
Icons are primary text colour, 1px lines on the 8x16 grid look; counts use the 18px straw badge from the Notification drawer section.

## Milk Drop sheet (Files > Share)
Mockups: share.html. Built on the existing `drop` commands only (docs/networking.md), so no new kernel work.
- Sending: the Share button (or dropping a file on a NEARBY name) opens a sheet, 520px wide, radius 14, window shadow, header 44 (title "Milk Drop", Esc hint on the right), below the Files toolbar's title bar.
- Device rows: 64 high, radius 12, 12px side margin, 12px between. Name bold, fingerprint in four groups of four (e.g. 3fa2 91bc 07de 44a1) in secondary text, a trust chip on the right (Known: matcha at 27%, New device: honey at 27%, KEY CHANGED: #B5473B with light text, Not receiving: latte and the row at 55% opacity with "open Milk Drop there"). Selected row: straw 20% fill and a 2px straw border.
- Data: `MILK_TABLE=1 drop list` (name, address, fingerprint, receiving, trust). Rescan while the sheet is open (about every 3 seconds; one scan is about 1.5 s) and show a small spinner while scanning.
- Footer: "Encrypted, checked with SHA-256." in secondary text (it said "not yet encrypted" until encryption landed; keep it honest if that ever changes), Cancel (secondary) and Send (primary) dialog buttons.
- Progress: a file chip, a 12px straw progress bar, and the numbers from `drop: [####......]  40%  416 KiB / 1024 KiB`. Cancel stops the transfer. `drop: sent ...` (exit 0) closes the sheet and shows the Received toast on the other machine; `drop: <reason>` (exit 1) shows the reason in an error row.
- Incoming (a milkOS-owned panel in the Powerbox style, 644 wide): "alice wants to send you bike.png" with the size, and for a new device a honey panel with the fingerprint at 4x text, "Check this matches what alice sees", and "Accepting trusts this device from now on." Buttons: Decline (secondary) and Trust alice and accept (primary). A known device shows no panel and the buttons Decline and Accept. Driven by `drop recv`, which prints "wants to send you" then waits at "accept? [y/N]"; the panel writes y or n.
- Key changed: a danger-coloured header ("bob's key has changed", "Sending is blocked until you decide"), the old and new fingerprints side by side, advice to compare them in person or by phone, and one danger button, Forget bob and re-verify (`drop forget bob`). Sending stays blocked until then.
- Received toast: the Notification drawer's toast with two pill buttons, Show (in Files) and Open. Files land in ~/Downloads.
- Nothing nearby: "No other milkOS machines found" with Look again and Close.

## Shield (design only, parked, not scheduled)
Mockups: shield.html. A read-only view of what each app can do and what it was refused. Nothing here needs new enforcement: it reads the capability sets and the audit log (docs/security.md). Any control that would change network access is drawn disabled and marked "parked, not scheduled". Do not queue engineering for this until the project owner says so. The first buildable version could be read-only: a title-bar button opening a panel (files, files you gave it, network, sound, clipboard, windows, recent activity), and Settings > Privacy with the audit log as sentences and an "intact" line from `audit verify`.

## What the mockup does not show
Hover/pressed states (lighten fills 4% / darken 4%), animation (none required for 1.0; Spaces can snap), cursor, lock screen, text selection colour (straw at 27%). Raise it with the design owner if one of these blocks you.
