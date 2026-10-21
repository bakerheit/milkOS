#!/usr/bin/env python3
"""Undo kernel log lines that landed inside echoed shell input (stdlib only).

  console_clean.py LOG > CLEAN

The serial console carries both msh (prompt + the echo of what a test types) and the
kernel's log. Kernel lines are printed whole, but asynchronous ones (timemachine's boot
snapshot, the audit log re-attaching, DHCP, ...) can arrive while a command is being
echoed, splitting it:

    msh:/$ moo2timemachine: boot: snapshot auto-... (generation 136)
    048 --selftest

Tests that look for `msh:/$ moo2048 --selftest` then miss it. This rejoins the echo
(`msh:/$ moo2048 --selftest`) and puts the kernel line on its own line right after it.
Only prompt lines (starting with `msh:`) are touched, and only at a known kernel log
prefix; CRs are dropped.
"""
import re
import sys

# Prefixes of kernel log lines (kprintln!) that can show up after the shell is running.
KERNEL = re.compile(
    r"(?:timemachine|milkfs|wm|fs|crash-writer|display|notes|pagecache|mem|fat|selftest): "
    r"|\[(?:audit|net|rng|login|exec|proc|virtio[a-z0-9-]*|audio|smp|demo|uart|gic|strace)\] "
)


def clean(text):
    lines = text.replace("\r", "").split("\n")
    out = []
    i = 0
    while i < len(lines):
        line = lines[i]
        moved = []
        if line.startswith("msh:"):
            start = line.find("$ ")
            start = start + 2 if start >= 0 else 0
            while i + 1 < len(lines):
                m = KERNEL.search(line, start)
                if not m:
                    break
                # The kernel line ran to the end of this line; the echo goes on below.
                moved.append(line[m.start():])
                i += 1
                line = line[: m.start()] + lines[i]
        out.append(line)
        out.extend(moved)
        i += 1
    return "\n".join(out)


if __name__ == "__main__":
    with open(sys.argv[1], "rb") as f:
        sys.stdout.write(clean(f.read().decode("utf-8", "replace")))
