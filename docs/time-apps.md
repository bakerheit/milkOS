# Calendar, Clock and the alarm daemon

Two windows (`/Apps/calendar/calendar`, `/Apps/clock/clock`, dock tiles from `/Apps/<app>/manifest`), a
background ringer (`alarmd`) and shell tools (`cal`, `alarm`, `timer`). The windows are built on
`user/appkit` (the SPEC theme, buttons, segmented control, switch, chips, text fields, rings).

Pure logic lives in `user/timekit` (no syscalls, unit-tested on the Mac); `user/timeio` is the
system side (data files, the local clock, notifications); `user/timetools` has `alarmd` and the
command-line tools. The Calendar and Clock windows are built on the same three crates.

Time is the RTC, which holds local time: everything is "local seconds since 1970-01-01 00:00",
with no time zones and no daylight saving.

## Files

All are plain text, one record per line, fields separated by tabs (tabs, newlines and backslashes
inside a field are escaped). Lines that do not parse are skipped and counted, never fatal, so a
hand-edited file cannot lock an app. Writes go to a sibling file and are renamed into place.

| File | Written by | Lines |
| --- | --- | --- |
| `~/Library/Calendar/<Name>.cal` (one per calendar) | Calendar, `cal` | `calendar name colour [hidden]`, then `event id date start end repeat[:every] until alert title notes` |
| `~/Library/Reminders/<Name>.rem` (one per list) | `remind` (Reminders app later) | `list name colour`, then `reminder id due time repeat[:every] until done alert title notes` |
| `~/Library/Clock/alarms` | Clock, `alarm`, alarmd (switches off "once" alarms) | `alarm id HH:MM days on\|off label` |
| `~/Library/Clock/timers` | Clock, `timer`, alarmd (removes finished ones) | `boot id`, then `timer id running\|paused deadline_ms\|remaining_ms total_ms label` |
| `~/Library/Clock/recent` | Clock | custom timer minutes, newest first |
| `~/.cache/milk/alarmd.log` | alarmd | `date time kind: title / subtitle`, restarted at 64 KiB |

Calendars: `colour` is straw, matcha, blue, honey or lav. With no files the three defaults
(Personal, Home, Work) are shown. Event ids are unique across all calendars. `start`/`end` are
`HH:MM` or `-` (all day); `repeat` is `none|daily|weekly|monthly|yearly`, with `:N` for every N;
`alert` is minutes before the start (0 = at the time; an all-day event alerts at 09:00). A monthly
event on the 31st happens on the last day of shorter months, a yearly 29 February on the 28th in
other years. A hidden calendar is not drawn but still alerts.

Reminders: `due` is a day or `-`, `time` is `HH:MM` or `-` (a due day without a time rings at 09:00),
`done` is `-` or the day it was completed, `alert` is 1 (rings) or 0. Completing a repeating reminder moves its
due day to the next one after today (skipping what was missed) instead of closing it; the last
occurrence (past `until`) closes it. alarmd rings a reminder once, when its time passes; done ones
never ring.

Alarms: `days` is a weekday mask (bit 0 Monday ... bit 6 Sunday); 0 means once, and the alarm
switches itself off after it rings. "Snooze 9 min" adds a once alarm.

Timers run on the monotonic clock (setting the wall clock does not move them). That clock starts
again at boot, so `timers` names the boot it was written in (`/run/boot-id`, a tmpfs file) and
running timers from an earlier boot are dropped; paused ones keep their remaining time.

**Who owns what.** alarmd is started by init, so it runs as root, but the files are the user's.
Whatever root creates in a user's folder (folders, the alarms and timers files alarmd rewrites)
is given to the owner of the parent folder (`timeio::write_atomic`, `mkdir_p`), so the user's apps
can still change it. The boot id file (`/run/boot-id`) is made by alarmd at start and read by
everyone; if it cannot be made, processes fall back to the wall clock minus the monotonic clock.

## Toasts, the menu bar and resizing

alarmd posts every ring as a toast with up to two buttons that run a command (docs/processes.md,
`notify ... action=<label>:<command>`), so they work with every window closed:

| Ring | Buttons |
| --- | --- |
| Alarm | Snooze 9 min (`/Commands/alarm snooze ID 9`), Open (the Clock) |
| Timer done | Again (`/Commands/timer <seconds>s <label>`), Open |
| Calendar alert | Open (the Calendar) |
| Reminder | Done (`/Commands/remind done ID`), Snooze 10m (`/Commands/remind snooze ID 10`) |

A reminder's snooze is the last column of its line (`YYYY-MM-DD@HH:MM`); it rings again then and
leaves the due day alone; completing the reminder clears it. A ringing alarm still opens the small
Snooze / Stop panel that plays the alarm sound.

alarmd also keeps the soonest running timer in the menu bar ("04:32", refreshed every second; the
desktop drops it 5 seconds after the last refresh, and alarmd clears it when no timer runs), so a
timer stays visible after the Clock window is closed. Clicking it opens the Clock.

Calendar, Activity Monitor, Disk Utility and Reminders are resizable (their layouts follow the
window; the Clock is not). The smallest sizes are larger than the spec's, because the sheets are
fixed size: Calendar 760x580, Activity Monitor 720x480, Disk Utility 760x560, Reminders 640x520.

## alarmd

Started by init. Once a second it reads the three files (no cache: an app's edit counts on the
next tick), rings what fell due in the last second as a desktop notification (`notify` on
`/dev/wm`) and a chime (`play -n system chime`), and logs it. Time that passed while the VM was
paused is caught up, at most ten minutes of it; a clock stepped backwards rings nothing.

## Tools

```
alarm [list] | add HH:MM [once|daily|weekdays|weekends|mon,wed,fri] [label] | add in 5m [label] | rm|on|off ID
timer [list] | DURATION [label] (5m, 1h30m, 90s) | pause|resume|reset|rm ID
remind [list [NAME]] | lists | add [--list NAME] [WHEN [HH:MM] | in 5m] TITLE [--repeat daily|weekly|monthly|yearly[:N]] [--until DATE] [--flag] | done|undone|flag|rm ID
cal | cal MONTH YEAR | cal list [DAYS] | cal add WHEN [HH:MM[-HH:MM]] TITLE [--repeat daily|weekly|monthly|yearly[:N]] [--until DATE] [--alert MIN] | cal add in 5m TITLE | cal rm ID
```

`in 5m` means the first whole minute at least five minutes from now (alarms and events have
minute resolution). WHEN is `today`, `tomorrow`, `+3d`, a weekday name or `YYYY-MM-DD`.

## The windows

**Calendar** (940x620): sidebar with a search field, the mini month, the calendar list (click a
name to show or hide it, Add calendar for a new one with any accent colour) and New event;
toolbar with Day / Week / Month / Year, the title, Today and previous / next. Month cells show
chips (a bar in the calendar colour, the colour at 27% as fill) and "+N more", which opens that
day. Week and day have hour rows (scroll for the night), an all-day strip and a now line; the
Year view has twelve mini months. Click a day to select it, double-click to make an event there
(double-click an hour slot to fill in the time). The event sheet has Title, When (date, start, end,
All day), Repeat (never, daily, weekly, monthly, yearly with every N and an until date), Alert
(none, at time, 5 min, 15 min, 1 hour, 1 day), Calendar and Notes; Return saves, Esc cancels.
Keys: arrows move the day, Return new event, `t` today, `d w m y` the views, `[` `]` previous and
next, Cmd/Ctrl-F search, Cmd/Ctrl-N new event.

**Clock** (600x560): Clock (analog face, the time at 4x), Alarms (rows with switches, the next ring
in the status bar, an edit sheet with time, weekdays and label), Stopwatch (laps, the fastest and
slowest marked with an arrow as well as colour) and Timer (a ring, presets, Custom, +1 min). A
ringing alarm opens a small panel with Snooze 9 min and Stop and plays the alarm sound every two
seconds. Cmd/Ctrl-1..4 switch tabs, Space starts the stopwatch.

Differences from docs/ui-concepts/SPEC-APPS.md, all because only the 8x16 bitmap font exists and
client windows have a fixed size: text is 16px everywhere (the spec's 12px chips are 18px high),
the Calendar is 620 high so it fits above the dock on a 1024x768 screen, its sheet is taller than
the spec's 400, the search field is in the sidebar (no room in the toolbar), times are 24-hour,
and world clocks are left out (the machine has no time zone setting). Stubbed until the
window-system engineer lands them: a Snooze/Open action on the toast, a menu bar item that shows
a running timer, and the Today card hook; alarmd and Clock work without them.

**Reminders** (780x620): smart lists Today (due today plus overdue), Scheduled (grouped by day) and
All, then your lists (Personal, Home, Work to start; Add list takes any accent colour). A quick-add
field takes a title followed by a day, a time and a repeat in plain words ("Call mum tomorrow at
5pm", "Water plants every 2 weeks", "Rent +3d 09:00"; `timekit::quickadd`), and a preview line shows
how it was understood before Return adds it. Click a row to select it and again to edit in place
(title, notes, date, time, repeat, list, alert); Space completes the selected row (a repeating one
moves to its next day), Delete removes it with a 5 second Undo, and completed ones are in a
collapsible group. alarmd rings each reminder once at its time (09:00 when it has a day only).
Missing until the window system lands them: the toast's Done and Snooze actions and the Today card.

## Tests

- `scripts/timekit-test.sh`: the unit tests (dates across centuries, leap days, repeat rules,
  alerts across midnight, alarm/timer/stopwatch behaviour, file round trips and junk lines).
- `scripts/apps-test.sh`: boots a desktop, opens Calendar and Clock, clicks through the views,
  makes an event in the window and checks the file with `cal` (screenshots are kept).
- `scripts/alarm-test.sh`: one boot rings an alarm, a timer and a calendar alert through alarmd;
  a second boot checks everything survived on the milkfs home folder.
