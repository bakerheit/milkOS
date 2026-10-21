milkOS @VERSION@
=============

milkOS is a small hobby operating system written from scratch in Rust. It runs in a
virtual machine on your Mac: a little desktop with a Finder-like Files app, Notes,
Terminal, Music, a browser and more, on its own file system with snapshots (Time
Machine style) that keeps what you save.

What you need
-------------
- A Mac with Apple Silicon (M1 or later) and macOS 13 or newer.
- QEMU, the free virtual machine program. The easiest way to get it:
    1. Install Homebrew from https://brew.sh (copy the one line it shows into Terminal).
    2. In Terminal, run:  brew install qemu

Starting it
-----------
Double-click "milkOS.command". The first time, macOS may say it can't check the file:
right-click (or Control-click) milkOS.command, choose Open, then Open again.

Two windows appear: the milkOS screen, and a Terminal window that is milkOS's console
(you can type commands there too, like "ls" or "help").

- Sound is off. To hear it, run it from Terminal as:  ./milkOS.command --sound
- To quit, close the milkOS window (or type Ctrl-A, then X, in the Terminal window).
  Anything saved in milkOS is kept for next time, in disks/milk.img.

Your files and the Mac
----------------------
- Your milkOS home folder (/home/milk) lives in disks/milk.img. Keep the whole milkOS
  folder together; to start over, replace disks/milk.img with a fresh copy from the zip.
- Anything you put in the "share" folder next to this file shows up inside milkOS as
  /Volumes/Mac (read-only: milkOS can look but not change it).
- milkOS can't reach your Mac or the internet: its network is switched off from the
  outside, so it's safe to poke around.

If something goes wrong
-----------------------
- "milkOS needs QEMU": install it as above.
- The window closes right away: open Terminal, drag milkOS.command into it, press
  Return, and read the message. Only one copy of milkOS can run at a time.
