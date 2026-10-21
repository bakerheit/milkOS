milkOS shared folder
====================

This folder (and only this one) is shared with milkOS over virtio-9p when you run
scripts/run.sh (or the other boot scripts). Inside milkOS it is /Volumes/Mac.

- Read-only by default. Boot with MILK_SHARE_RW=1 to let milkOS write here too.
- Drop a file here and it shows up in milkOS right away, e.g. a user program you built:
  copy it here and run /Volumes/Mac/<name> from msh.
- Everything except this README is ignored by git.
