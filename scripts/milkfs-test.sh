#!/usr/bin/env bash
# Host tests of the milkfs crate (<repo>/milkfs) on the Mac itself, no VM: the file system on
# a disk that is a byte array, with a fake clock, a seeded RNG and disks that lose power.
# Format/mount/read/write, B+tree splits and merges, check and repair of injected damage,
# reflinks and dedupe, snapshots, clones, diff, send/receive, scrub, encryption (unlock,
# wrong key, rekey), in-place encrypt/decrypt and the v0 upgrade, all through crashes; the
# on-disk format checked against an image main's kernel wrote. Prints "milkfs-unit: N passed"
# and fails if none ran. See scripts/hosttest.sh; docs/filesystem.md ("Host tests").
exec "$(dirname "$0")/hosttest.sh" milkfs milkfs-unit
