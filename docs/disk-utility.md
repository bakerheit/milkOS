# Disk Utility

`/bin/diskutil` (dock tile from `/System/Applications/diskutil/manifest`), following
docs/ui-concepts/SPEC-DISK-REMINDERS.md section 1. The window is `user/diskutil`; everything that can
be decided without a syscall is in `user/diskkit` (unit tests: `scripts/diskkit-test.sh`). No
kernel changes: it reads and writes `/sys/fs/milkfs/*` and reads `/proc/mounts`.

## What it does
- **Sidebar:** the primary volume, its clones, the Mac share, the in-memory root; a locked
  encrypted volume shows a padlock and "locked".
- **Overview:** usage bar, fact cards, the Time Machine card (Snapshot now, Settings sheet: on/off,
  interval, keep, daily), a health card, the records meter at 24 of 30.
- **Snapshots:** table (name, created, kind from the name, clones), Diff against live (A/M/D/R
  chips; click a row to open its folder in Files), Clone..., Roll back..., Send..., Delete...
  (disabled with the reason while a clone is built on it), Restore from file...
- **Clones:** a card per clone with Open in Files, Keep... and Discard...; New clone...
- **Health:** scrub (start, stop, progress, speed 16/32/64 MB/s), schedule (12 hours, 7 days, off),
  errors with paths, and a structure check (Check, Repair...).
- **Security:** encryption status, Change passphrase..., Lock now (drawn disabled: there is no lock
  call), the recovery-key note, and the backup warning with Back up...
- **Back up / Restore:** Send writes `snap send` to a stream file in `/Volumes/Mac/backups` (the
  folder must be writable: milkOS started with `MILK_SHARE_RW=1`; otherwise the sheet says so and
  Send stays disabled), after an "I understand" box about the missing encryption. Restore picks a
  stream there and receives it; "Replace existing" goes through the confirm sheet.
- **One confirm sheet** for every destructive action (Roll back, Delete snapshot, Discard clone, Keep
  clone, Repair, Replace): a Danger header with the question, the consequence in numbers, and
  Cancel as the default (Return and Esc cancel); the verb button is Danger and never the default.
  Keep and Repair offer "take a snapshot first".

Long writes (send, receive, check, repair) run in a forked child so the window stays alive; the
sheet's result is the kernel's own one-line report. A wrong passphrase says "That was not the
passphrase", never an errno; other errors give a sentence and "Details: error N".

## Where it differs from the spec, and why
- **No "snapshot first" box for Roll back or Replace.** Both delete every snapshot newer than the
  target (like `zfs rollback -r`), which includes the safety snapshot, so the box would promise
  something the kernel takes away. The sheet says how many newer snapshots go and to use Send first.
- **Encryption status** is shown as "not reported" for an unlocked volume: `info` does not say whether
  the volume is encrypted (diskkit already reads an `encryption:` line if the kernel adds one).
- **Privilege:** writes to the control files are made as the user (the `admin` group may write them).
  Error 13 reads "needs authorisation, and the system cannot ask for it yet" until the
  authorisation panel and ticket exist.
- **Other volumes:** name and kind only (no statfs for them yet). Free space of the Milk disk comes
  from `info`; the in-memory root shows `Shmem`.
- 900x620 instead of 640 high (it must clear the dock), 16px text, a wider confirm sheet (680).
