#!/usr/bin/env bash
# Host tests of the rpikit crate (<repo>/rpikit) on the Mac itself, no VM: the device tree
# reader against a hand-written Raspberry Pi 4 tree (memory banks, spin-table and PSCI cpus,
# command line, compatible) and against damaged ones, `milk.<name>=<value>` command line
# options, VideoCore mailbox messages (layout, hostile answers, the framebuffer request and
# its sanity checks), SD command and SDHCI register encoding, CSD capacities and clock
# dividers. Prints "rpikit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" rpikit rpikit-unit
