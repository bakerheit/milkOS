# Disk Utility

`/Apps/diskutil/diskutil` (dock tile from `/Apps/diskutil/manifest`), following
docs/ui-concepts/SPEC-DISK-REMINDERS.md section 1. The window is `user/diskutil`; everything that can
be decided without a syscall is in `user/diskkit` (unit tests: `scripts/diskkit-test.sh`). No
kernel changes: it reads and writes `/System/Kernel/fs/milkfs/*` and reads `/proc/mounts`.

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
- **Back up / Restore:** Send writes `disks send` to a stream file in `/Volumes/Mac/backups` (the
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

## Encrypted backups, and spare disks

When the system lists its disks (`/System/Kernel/fs/milkfs/disks`), a backup of an encrypted volume is encrypted
by default: the Back up sheet asks for a passphrase twice (`send` gets it on a second line) and offers
"Not encrypted" as a choice (`plaintext`), which brings back the warning and the "I understand" box.
Restore has an optional passphrase field (`receive` gets it on a second line); a wrong or missing one
reads "The passphrase is wrong or missing for this backup". On a system without that file the sheet is
the old one (unencrypted, with the warning). The sidebar also lists unmounted milkfs volumes and
disks that are not milkfs. Formatting and encrypting a spare disk (the `mkfs` control) is not in the
window yet; it needs a spec addendum from the UI lead first.

## Where it differs from the spec, and why
- **No "snapshot first" box for Roll back or Replace.** Both delete every snapshot newer than the
  target (like `zfs rollback -r`), which includes the safety snapshot, so the box would promise
  something the kernel takes away. The sheet says how many newer snapshots go and to use Send first.
- **Lock now** is enabled for an encrypted volume that is not the one the session lives on (a vault, a
  backup disk): `/System/Kernel/fs/milkfs/lock`. The Milk disk itself is refused by the system, so its button is
  disabled with the reason. A refusal names the programs using the volume ("pid 16 (sleep): 1 open
  file"), read from `/System/Kernel/fs/milkfs/error`; every failed control write shows its real reason there
  with "Details: lock EBUSY (error 16)".
- **Usage of every volume** comes from `statfs` (FAT, the Mac share, other milkfs volumes, the
  in-memory root); the Milk disk's usage still comes from `info`. Encryption state, cipher, KDF and
  key slots come from `info`'s `encryption:` lines.
- **Privilege:** writes to the control files are made as the user (the `admin` group may write them).
  An open that fails with error 13 reads "needs authorisation, and the system cannot ask for it yet"
  until the authorisation panel and ticket exist.
- 900x620 instead of 640 high (it must clear the dock), 16px text, a wider confirm sheet (680).
