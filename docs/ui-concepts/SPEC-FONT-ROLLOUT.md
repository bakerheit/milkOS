# milkOS: one font in every app (the rule, and what each app changes)

Read `SPEC.md` (Type) first. Written for **the app engineer** and **the window-system engineer**. It follows the 2026-10-23 review of main: Files, Browser, Settings, Music, Today, Skim and the notification drawer use the system font; Calendar, Clock, Disk Utility, Activity Monitor, Reminders, Preview, Milk Store, Paint and the Text Editor's status bar still draw in Unifont 8x16, so the desktop looks like two systems. This page makes it one. It does not pick a font: the rule is "whatever Settings > Appearance > Font says".

## The rule
1. **UI text is the system font.** Labels, buttons, tabs, segmented controls, lists, table cells, sidebars, status bars, tooltips, empty states, errors. 13px in a 16px line, espresso; secondary text cocoa; disabled latte. Bold is the face's real Bold.
2. **Headings and big numbers are the system font at its baked sizes:** Heading 26px bold (scale 2) for window titles inside the content ("Milk", "Today", "October 2026"), Display 52px bold (scale 4) for the Clock time and big tile letters. No Unifont at scale 2 for headings any more.
3. **Unifont 8x16 is only for monospace content**, where the columns carry meaning:
   - Terminal and the console; Notes' editor and the Text Editor's body and line-number gutter; the Versions file preview.
   - The Milk Store's command log box.
   - The Disk Utility recovery key (8 groups of 4 at 2x) and any command or device path shown as code (`snap mkfs vdb`).
   - Number columns that update live and must not jitter: Activity Monitor's PID, CPU %, Memory and Threads cells (right-aligned, fixed width). All other numbers are the system font.
4. **Section labels** ("MUSIC", "SYSTEM SOUNDS", "LISTS", "VOLUMES", "CALENDARS"): the system font, 13px, uppercase, cocoa. This is what Music and Today already do; it replaces the old "8x16 uppercase" wording.
5. **Row height and layout do not change.** The system font's line is 16px like Unifont's, so every spec'd height, padding and radius stays. Text gets narrower, so nothing that fit before can overflow. Truncate with "~" as before (the face covers U+0020 to U+00FF only).
6. Fixed-width digits where numbers line up in a grid (Calendar's mini month, Clock's stopwatch, the Disk Utility stat tiles): draw each number inside a cell of fixed width, right-aligned or centred, instead of relying on the face's digit width.
7. A new window or sheet must never hard-code Unifont for UI text. If the user GUI library cannot draw the system font yet, that is the missing piece, not a reason to keep the bitmap font (see "Prerequisite").

## Prerequisite (engineering)
`SPEC-APPS.md` already says it: the user GUI library has plain Unifont and "does not follow the system font yet". The apps below cannot change until it has a text call that draws the current system family (13, 26 and 52px, regular and bold) and follows a font change in Settings without a restart. How the baked glyph data reaches userland (shared from the kernel's tables, or a kernel text service) is for the engineers to choose. Nothing in this page depends on which of the five fonts is the default.

## Per app: what changes
| App | UI font | Stays Unifont |
|---|---|---|
| Calendar | month title (Heading), tabs Day/Week/Month/Year, Today and arrows, day numbers, mini month, calendar list, "Add calendar", "New event", status line, search | none |
| Clock | tabs, date line, note line, alarm and timer lists and buttons; the time is Display bold | none |
| Disk Utility | everything: sidebar volumes, tabs, titles (Heading), stat tiles, Time Machine card, health card, sheets and their buttons | the recovery key and any shown command |
| Activity Monitor | tabs, column headers, process names, State and Sandbox chips, search, Quit, status bar | PID, CPU %, Memory, Threads cells |
| Reminders | sidebar, list title (Heading), the add field, rows, counts, empty state | none |
| Preview | toolbar buttons, empty state card, Info panel, status | none |
| Milk Store | title (Heading), stats line, package list, buttons, status chips, hints | the command log box |
| Paint | toolbar, size dots, Clear and Save, the hint line | none |
| Text Editor | status bar (Ln, Col, UTF-8, LF, Spaces, Wrap, Plain Text, Saved), menus | body and gutter |
| Files, Browser, Settings, Music, Today, Skim, drawer, Welcome | already the system font | Terminal-style panes only |

## Checks (for review of each app)
- No Unifont glyph on any control, label or heading in light and dark, at 13px and at every Settings > Font choice (check the widest, Atkinson Hyperlegible Next, and the narrowest).
- Switching the font in Settings changes the app live (or on its next window; say which).
- Every title bar, tab and button still passes the SPEC contrast numbers (the colours do not change).
- A screenshot of each app next to Files and Settings looks like one system: same size, same weight, same line height.

## Needs (not decided here)
- Whether the live-update on a font change is per redraw or per window open (engineering).
- Whether the Activity Monitor's number columns can use the face's own digits once the baked digits are known to be fixed width; if they are, the columns go to the system font and Unifont is not needed there at all.
