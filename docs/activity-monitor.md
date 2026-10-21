# Activity Monitor

`/bin/activity` (dock tile from `/System/Applications/activity/manifest`). The window is
`user/activity`; everything that can be decided without a syscall is in `user/sysview` and has
unit tests (`scripts/sysview-test.sh`). It follows docs/ui-concepts/SPEC-APPS.md, section 1.

Tabs (Cmd/Ctrl-1 to 5): **Processes, CPU, Memory, Disk, Security**. It refreshes every second;
click "updates every 1 s" in the summary bar to step through 0.5, 1, 2, 5 seconds and paused.

## What it reads

| Tab | Files |
| --- | --- |
| Processes | `/proc/<pid>/status` (Name, State, PPid, VmRSS, Threads, CpuNs, StartNs, and `Caps` once the kernel has it), `/proc/<pid>/exe`, `cwd`, `fd` |
| CPU | `/proc/cpuload` (per core), `/proc/loadavg`, `/proc/interrupts` |
| Memory | `/proc/meminfo`, the processes' VmRSS |
| Disk | `/proc/diskstats` (sectors read and written), `/sys/fs/milkfs/info` (blocks free), `Shmem` in meminfo for the in-memory root |
| Security | `/proc/audit`, `/proc/audit_status`, and `audit verify` run as a program |

CPU percent is the change in `CpuNs` between two looks over the wall time between them, so 100% is
one whole core (a process using two cores shows 200%). A pid that was reused (different
`StartNs`) starts again at 0.

## Quit

Select a row and press Quit (or Delete): "Quit moo2048?" with Cancel and Quit. Quit sends SIGTERM.
The sheet waits for the process to go away; after five seconds it offers Force Quit (SIGKILL).
`init` and pid 1 can not be quit: the button is disabled and the detail panel says why.

## Security

The audit log as sentences, newest first, with filter chips (All, Refused, Files, Programs, Installs,
Milk Drop), search, and the dot colours from the spec. Click a row to see the raw record. The banner
runs `audit verify` on first view and on "Verify again": "Log is intact" with the record count and
time, or "The log has been changed" with the first bad record. "Security" in a process's detail
panel jumps here filtered to that pid. Events the app does not know show as their raw name.
The mapping from raw event to sentence is `sysview::audit::describe`.

## Not there yet

- **Sandbox column**: hidden until `/proc/<pid>/status` has a `Caps:` line. When it does, the column
  shows "Sandboxed" for a process whose `fs=` is not `all`, and "System" for init (the parser in
  `sysview::proc::sandbox_label` guesses at the line's format: `fs=<list|all> net=...`; adjust it
  when the kernel's real format is fixed). The detail panel's "Can read" and "Network" lines wait
  for the same line.
- **Free space per volume**: only the Milk disk (from milkfs `info`) and the in-memory root; other
  volumes need a statfs call.
- **Text size**: 16px text everywhere (the only font), so rows and chips are a little taller than
  the spec's; the window is 800x600 so it clears the dock on a 1024x768 screen.
