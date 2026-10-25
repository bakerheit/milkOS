#!/usr/bin/env python3
"""Write a small Raspberry Pi 4 style device tree blob (python3 stdlib only).

    scripts/pi_dtb.py OUT.dtb [--cmdline TEXT] [--psci hvc|smc] [--cpus N] [--ram-mb N] [--bank START:SIZE]

qemu's `-M raspi4b` has no device tree of its own, and the real one is the firmware's (it
belongs to the Pi's boot partition, not to this repository), so the tests build their own:
the shape of the stock tree (root `compatible` raspberrypi,4-model-b / brcm,bcm2711, two address
cells and one size cell, /chosen, /memory, four cortex-a72 cpus released through the spin table at
0xd8, 0xe0, 0xe8, 0xf0) with nothing else in it. qemu fills in /chosen/bootargs from `-append` and
/memory from its RAM size when it loads the tree. With --psci the cpus use PSCI instead of the spin
table (for host-side experiments).

Unless the command line names `milk.fbsize=` the tree adds `milk.fbsize=1024x768`: qemu's raspi4b answers the
mailbox's "physical display size" with its own 640x480 default (there is no monitor behind it), and the kernel
follows the firmware's display size (docs/raspberry-pi.md, "HDMI displays"), so the tests pin the size they were
written for. A test of the automatic choice passes `milk.fbsize=auto`.
"""
import argparse
import struct


class Fdt:
    def __init__(self):
        self.struct = bytearray()
        self.strings = bytearray()

    def _word(self, v):
        self.struct += struct.pack(">I", v)

    def _pad(self):
        while len(self.struct) % 4:
            self.struct.append(0)

    def begin(self, name):
        self._word(1)
        self.struct += name.encode() + b"\0"
        self._pad()

    def end(self):
        self._word(2)

    def prop(self, name, data):
        key = name.encode() + b"\0"
        off = self.strings.find(key)
        if off < 0:
            off = len(self.strings)
            self.strings += key
        self._word(3)
        self._word(len(data))
        self._word(off)
        self.struct += data
        self._pad()

    def cells(self, name, *vals):
        self.prop(name, b"".join(struct.pack(">I", v) for v in vals))

    def string(self, name, *vals):
        self.prop(name, b"".join(v.encode() + b"\0" for v in vals))

    def blob(self):
        self._word(9)
        off_rsv = 40
        off_struct = off_rsv + 16
        off_strings = off_struct + len(self.struct)
        total = off_strings + len(self.strings)
        header = struct.pack(
            ">10I", 0xD00DFEED, total, off_struct, off_strings, off_rsv,
            17, 16, 0, len(self.strings), len(self.struct))
        return header + b"\0" * 16 + bytes(self.struct) + bytes(self.strings)


def build(cmdline, psci, cpus, ram_mb, banks=(), ram_bytes=0):
    f = Fdt()
    f.begin("")
    f.string("compatible", "raspberrypi,4-model-b", "brcm,bcm2711")
    f.string("model", "Raspberry Pi 4 Model B")
    f.cells("#address-cells", 2)
    f.cells("#size-cells", 1)
    f.begin("chosen")
    f.string("bootargs", cmdline)
    f.end()
    f.begin("memory@0")
    f.string("device_type", "memory")
    f.cells("reg", 0, 0, ram_bytes or ram_mb << 20)
    f.end()
    # More RAM banks (the 4 GB and 8 GB boards list several; qemu rewrites only memory@0).
    for start, size in banks:
        f.begin("memory@%x" % start)
        f.string("device_type", "memory")
        f.cells("reg", start >> 32, start & 0xFFFFFFFF, size)
        f.end()
    f.begin("cpus")
    f.cells("#address-cells", 1)
    f.cells("#size-cells", 0)
    for i in range(cpus):
        f.begin("cpu@%d" % i)
        f.string("device_type", "cpu")
        f.string("compatible", "arm,cortex-a72")
        f.cells("reg", i)
        if psci:
            f.string("enable-method", "psci")
        else:
            f.string("enable-method", "spin-table")
            f.cells("cpu-release-addr", 0, 0xD8 + 8 * i)
        f.end()
    f.end()
    if psci:
        f.begin("psci")
        f.string("compatible", "arm,psci-1.0")
        f.string("method", psci)
        f.end()
    f.end()
    return f.blob()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--cmdline", default="")
    ap.add_argument("--psci", choices=["hvc", "smc"])
    ap.add_argument("--cpus", type=int, default=4)
    ap.add_argument("--ram-mb", type=int, default=1024)
    ap.add_argument("--ram-bytes", type=lambda x: int(x, 0), help="size of memory@0 in bytes (instead of --ram-mb)")
    ap.add_argument("--bank", action="append", default=[], metavar="START:SIZE",
                    help="another RAM bank (hex or decimal), e.g. 0x40000000:0x40000000; repeatable")
    a = ap.parse_args()
    if "milk.fbsize=" not in a.cmdline:
        a.cmdline = (a.cmdline + " " if a.cmdline else "") + "milk.fbsize=1024x768"
    with open(a.out, "wb") as fh:
        banks = [tuple(int(x, 0) for x in b.split(":")) for b in a.bank]
        fh.write(build(a.cmdline, a.psci, a.cpus, a.ram_bytes // (1 << 20) if a.ram_bytes else a.ram_mb, banks, a.ram_bytes))


if __name__ == "__main__":
    main()
