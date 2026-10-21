# Calendar, Clock and the alarm daemon

Pure logic lives in `user/timekit` (no syscalls, unit-tested on the Mac); `user/timeio` is the
system side (data files, the local clock, notifications); `user/timetools` has `alarmd` and the
command-line tools. The Calendar and Clock windows are built on the same three crates.

Time is the RTC, which holds local time: everything is "local seconds since 1970-01-01 00:00",
with no time zones and no daylight saving.

## Files

All are plain text, one record per line, fields separated by tabs (tabs, newlines and backslashes
inside a field are escaped). Lines that do not parse are skipped and counted, never fatal, so a
hand-edited file cannot lock an app. Writes go to a sibling file and are renamed into place.

| File | Written by | Line |
| --- | --- | --- |
| `~/Library/Calendar/events` | Calendar, `cal` | `event id date start end repeat[:every] until alert title notes` |
| `~/.config/milk/alarms` | Clock, `alarm`, alarmd (switches off "once" alarms) | `alarm id HH:MM days on\|off label` |
| `~/.config/milk/timers` | Clock, `timer`, alarmd (removes finished ones) | `timer id running\|paused deadline_ms\|remaining_ms total_ms label` |
| `~/.cache/milk/alarmd.log` | alarmd | `date time kind: title / subtitle`, restarted at 64 KiB |

`start`/`end` are `HH:MM` or `-` (all day); `repeat` is `none|daily|weekly|monthly|yearly`, with
`:N` for every N; `alert` is minutes before the start. A monthly event on the 31st happens on the
last day of shorter months, a yearly 29 February on the 28th in other years. `days` on an alarm is
a weekday mask (bit 0 Monday ... bit 6 Sunday); 0 means once, and the alarm switches itself off
after it rings.

## alarmd

Started by init. Once a second it reads the three files (no cache: an app's edit counts on the
next tick), rings what fell due in the last second as a desktop notification (`notify` on
`/dev/wm`) and a chime (`play -n system chime`), and logs it. Time that passed while the VM was
paused is caught up, at most ten minutes of it; a clock stepped backwards rings nothing.

## Tools

```
alarm [list] | add HH:MM [once|daily|weekdays|weekends|mon,wed,fri] [label] | add in 5m [label] | rm|on|off ID
timer [list] | DURATION [label] (5m, 1h30m, 90s) | pause|resume|reset|rm ID
cal | cal MONTH YEAR | cal list [DAYS] | cal add WHEN [HH:MM[-HH:MM]] TITLE [--repeat daily|weekly|monthly|yearly[:N]] [--until DATE] [--alert MIN] | cal add in 5m TITLE | cal rm ID
```

`in 5m` means the first whole minute at least five minutes from now (alarms and events have
minute resolution). WHEN is `today`, `tomorrow`, `+3d`, a weekday name or `YYYY-MM-DD`.

## Tests

- `scripts/timekit-test.sh`: the unit tests (dates across centuries, leap days, repeat rules,
  alerts across midnight, alarm/timer/stopwatch behaviour, file round trips and junk lines).
- `scripts/alarm-test.sh`: one boot rings an alarm, a timer and a calendar alert through alarmd;
  a second boot checks everything survived on the milkfs home folder.
