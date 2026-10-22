# Review: notification drawer and window tabs builds (October 2026)

Reviewed from `docs/screenshots` (drawer-arrive, drawer-open, drawer-keys, drawer-empty, dark-drawer-open, tabs-drop, tabs-merge, tabs-mixed, tabs-terminal, dark-tabs-merge) against `SPEC.md`. Geometry is measured from the pixels; contrast is text against its background.

## Verdict
Both builds follow the spec closely. Two real bugs and a handful of spec updates, listed below. The spec has been updated where the build is right and the spec was wrong or silent.

## Notification drawer
**Matches the spec (measured):** panel 316 wide with its left edge at x696 and right edge at x1012 (12px margin), top y36, bottom y756 (12px margin), above the dock; filter chips 26px high at y82; cards, group headers with real apps, the selected card outline, the count badges on the bell and the dock icon, the toast (Mum, "now"), the Focus tint and moon when on, the empty state, and the dark theme. Text contrast in both themes is 6.3:1 or better everywhere in the drawer (primary 12.7:1, secondary about 6.5:1).

**Findings**
| # | Finding | Priority | Action |
|---|---|---|---|
| 1 | The last row says "Do Not Disturb", but Focus is the single setting and the Control centre calls it Focus. Two names for one switch. | P1 | Spec updated: the row is labelled "Focus" with the sub-line "On until 12:00" while on. Change the label in the build. |
| 2 | Truncated text ends in "~" ("Screenshot 2026~"). It reads like a typo. | P2 | Use a real ellipsis (U+2026) if the font draws it, else keep "~". Spec notes this. |
| 3 | The header "Notifications" is heavier than the mockup. | P2 | Fine as built. No change. |
| 4 | The test notifications in the screenshots and the demo script use a real person's first name as a fake sender. The repo is public. | P1 | Use neutral fake names (Sam, Robin) in the demo script and regenerate the screenshots. |

## Window tabs
**Matches the spec:** the 38px strip, tab height 32 starting at y6, 10px rounded tops, the active tab joining the content, the close x on the right of every tab, the "+" button, the Terminal tab strip colours (#4A3C34 bar, #3A2E29 active, #BEA890 inactive, 4.63:1 on the bar), and the dark theme. Terminal tabs "1 msh" and "2 msh" read well.

**Findings**
| # | Finding | Priority | Action |
|---|---|---|---|
| 1 | **Bug.** A Terminal tab merged into a light window draws its close x in the light cocoa (#6B5846) on the dark Terminal fill: **1.94:1**. In the dark theme and in the standalone Terminal it is correct (#BEA890, 5.74:1). | P0 | The x must take its colour from the tab it sits on. Spec clarified. |
| 2 | **Spec error, mine.** The drop target label "Drop to add as a tab" is straw text on a straw tint: **1.96:1**. The build copied the spec. | P0 | Label is now primary text, bold, with the dashed straw border carrying the colour. Spec updated. The chip is also too narrow in the build (the label is clipped at its right edge): make it at least 176px wide. |
| 3 | Keys: the build keeps the Terminal's Cmd-T and Cmd-W instead of the spec's Ctrl-T and Ctrl-W. | P2 | Spec updated to Cmd-T, Cmd-W, Cmd-1..9 (Ctrl-Tab for next). The build is right. |
| 4 | Known limit: Paint inside a tabbed window does not resize (its toolbar stops short of the window width and its hint text is clipped at the bottom). | P1 | Already known (no client resize yet). Until then, give client windows a minimum size equal to their content and do not allow them into a tab group smaller than that. |
| 5 | Dark theme: the inactive Terminal's text is dimmed to #8C7764 on #3A2E29, **3.08:1** (the light theme's dimmed text is 6.11:1). | P1 | Use #BEA890 (5.74:1), or dim the whole window with a veil instead of recolouring the text. Spec rule added. |

## Next
Send updated screenshots when the P0 fixes land and I will re-check them. After that: app icons and the UI font plan.
