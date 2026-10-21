# UI review of the shipped desktop (October 2026)

Reviewed from the screenshots in `docs/screenshots` (the contact sheet and the individual PNGs) against `SPEC.md`. Contrast numbers are measured from the actual pixels (text colour against the background around it), not guessed. Priorities: **P0** fails an accessibility bar or contradicts the spec, **P1** is a clear gap in how it works, **P2** is polish.

## Summary
The build is very close to the spec: menu bar 28, dock 64 at 12px from the bottom, window radius 12 with a 32px title bar, glass toast and dock, the wallpaper details, the Welcome tour, the Open panel wording, Versions, Skim and Spaces all read well. The gaps are in contrast on filled buttons and faint text, in Files (no toolbar), in the menu bar (nowhere to open the notification drawer from), and in the dock icons. The most valuable thing the desktop does not yet do is *show* its unusual features (sandbox, versions, audit, Milk Drop) to someone who never opens the terminal. Mockups for that are in `share.html` (Milk Drop sheet) and `shield.html` (read-only app permissions, design only, parked).

## P0: accessibility and spec
| # | Where | Finding | Measured | Fix |
|---|---|---|---|---|
| 1 | Welcome tour (all six buttons), Versions "Restore this version", every filled straw pill | Cream text on a straw fill | 2.42:1 (needs 4.5) | New Buttons section in SPEC: straw fill with ink text, 5.33:1. Hover 6.04, pressed 4.63. |
| 2 | Files status bar path ("~/Documents") | Latte (the disabled colour) used for real information on cream | 1.99:1 | Secondary text (cocoa). SPEC Files section. |
| 3 | Today, Notes card "Not edited yet" | Latte on the card | 1.81:1 | Secondary text. Rule: latte is for disabled things only. |
| 4 | Versions, row meta text on the selected row | Secondary text on a selected row | 3.53:1 | Primary text on selected rows (already a SPEC rule, not yet applied here). |
| 5 | Files sidebar section labels (FAVOURITES, DEVICES) | Secondary text on the sidebar | 4.40:1 | Fixed by the cocoa token change in SPEC (5.45:1). The screenshots still show the old colour, so this waits on that change landing. |
| 6 | Disabled buttons (Open before a file is chosen) | Latte on oat, looks the same as a normal Cancel | 1.73:1 (exempt) | Allowed, but never the only signal. SPEC Buttons: disabled has a 1px sand border and a not-allowed cursor, and the button label stays. |

Passing, for reference: Powerbox Cancel 10.58, Welcome "Get started" 12.90, Music subtitle 5.37, Welcome body 5.06, Today labels 4.69, Skim result subtitle 8.55.

## P1: how it works
1. **Files has no toolbar.** No back or forward, no path bar, no search, no view switch, no Share. An "up" pill sits in the status bar corner. The sidebar says "milk" and "Milk" (indistinguishable) and lists Time Machine as a device. There is no empty-folder state and no sign of what is selected. Proposal in SPEC ("Files window"): a 40px toolbar, a path bar replacing "up", a NEARBY group for Milk Drop devices, clearer device names, a clock button for Time Machine, and a status bar that says how many are selected.
2. **The menu bar has nowhere for the notification drawer.** The right side shows only the CPU meter and the clock. There is no bell, no Focus indicator, no network or sound icon, so Control centre and the drawer are only reachable by shortcut. Proposal in SPEC ("Menu bar status area"). The bell has to exist before the drawer ships.
3. **Dock icons are hard to tell apart.** Browser is a crosshair-like globe, the Store tile looks like a window, Control centre looks like a clock, Spaces is four tiles. There is no separation between apps and utilities and no Downloads or Trash. Next task: an icon set and a simple system for them.
4. **Today disagrees with the dock.** The Apps card shows 4 apps while the dock has 9. The greeting hard-codes a name; it should use the account name. "0h 00m" on a fresh boot looks broken: show nothing until a minute has passed.
5. **The Welcome tour is announced twice.** A toast ("Welcome to milkOS, Ctrl+Space: search") appears over the corner of the Welcome window while it is open. Suppress the toast while the tour is up.
6. **Music** puts "(no sound device: playing silently)" inside the track subtitle. That is a status, not a title: show it as a separate chip under the artwork.
7. **Settings** has only Appearance filled in. Missing and expected: Notifications and Focus, Keyboard and shortcuts (the shortcuts are only documented in the README), Network and Milk Drop (name, fingerprint, trusted devices), and Privacy (see `shield.html`).
8. **The strongest features are invisible.** Per-file versions, snapshots, the sandbox and Open panel, the audit log, try-before-install and Milk Drop are impressive but you only meet them in the terminal or the one-time Welcome tour. See `share.html` and `shield.html`.

## P2: polish
- Two type faces are in use (a rounded pixel face for headings and the Unifont-like body) but the spec does not name them or their sizes. A short type plan will keep new screens consistent. Planned.
- The Skim "Control centre" result icon is a clock-like circle; use the same glyph as the menu bar item once it exists.
- Spaces uses a lot of empty space when there are two windows; a larger scale would help but it is fine.
- Empty states are missing in Files, Notes and Music (nothing to play). Draw one calm line and one action each.
- Window titles for the Terminal read "andrew: msh". Fine, but if the account name changes make sure the title follows.

## What I would do next
1. Apply the Buttons section everywhere (P0 1, 4, 6). Small and mechanical.
2. Files toolbar and the menu bar status area (P1 1, 2), then the notification drawer's bell.
3. Milk Drop sheet (`share.html`), built only on the `drop` commands.
4. Dock icon set (P1 3) and the type plan.
5. Shield stays design only until the project owner decides.
