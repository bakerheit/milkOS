#!/usr/bin/env python3
"""Write a small SD card image with an MBR and one FAT32 partition that holds MILKLOG.TXT (python3 stdlib only).

    scripts/pi_fat32.py OUT.img [--size 64M] [--log-size 256K] [--fragment] [--defect KIND] [--layout OUT.json]

This is what scripts/pi-qemu-test.sh and scripts/pi-log-test.sh give qemu's raspi4b as its SD card, so the
kernel's boot log writer (kernel/src/rpi/logfile.rs, docs/raspberry-pi.md) has a card shaped like Raspberry Pi
OS's boot partition: an MBR whose first partition is FAT32 (type 0x0c), 512-byte sectors, one sector per cluster,
a root directory with a short-name file, a file with a long name (long-name entries then its 8.3 alias), a deleted
entry, a volume label, a big file full of recognisable noise (so "nothing else was written" is checkable), and the
pre-created log file: a header line, then spaces (what scripts/pi-logfile.sh makes).

  --fragment      the log file's clusters are scattered in runs of 8 with another file's clusters between
  --defect KIND   a card the writer must refuse and leave untouched:
                  loop (the chain runs back into itself), short (the chain ends early), long (the chain runs on
                  past the size), range (a cluster number outside the volume), free (a chain through a free
                  cluster), nofile (no MILKLOG.TXT), nombr (no partition table), tiny (the size is under 4 KiB),
                  v1 (the file holds a format 1 log), foreign (the file holds someone's text)
  --layout FILE   write a JSON description: the partition, the data area, and the log file's extents, which
                  is how a test knows which sectors the kernel was allowed to change

The size must be a power of two (qemu's SD card model wants that). Deterministic: the same arguments give the same bytes.
"""
import argparse
import json
import struct
import sys

SECTOR = 512
PART_START = 2048
RESERVED = 32


def size_arg(s):
    s = s.strip().upper()
    mult = {"K": 1024, "M": 1024**2, "G": 1024**3}.get(s[-1], 1)
    return int(s[:-1] if s[-1] in "KMG" else s) * mult


class Noise:
    """A small deterministic byte generator (xorshift)."""

    def __init__(self, seed):
        self.x = seed

    def block(self, n):
        out = bytearray()
        while len(out) < n:
            self.x ^= (self.x << 13) & 0xFFFFFFFFFFFFFFFF
            self.x ^= self.x >> 7
            self.x ^= (self.x << 17) & 0xFFFFFFFFFFFFFFFF
            out += struct.pack("<Q", self.x)
        return bytes(out[:n])


def build(a):
    total_sectors = a.size // SECTOR
    part_sectors = total_sectors - PART_START
    # Solve for the FAT size: clusters = (part - reserved - 2*fat) (one sector per cluster).
    fat = 1
    while True:
        clusters = part_sectors - RESERVED - 2 * fat
        need = (clusters + 2 + 127) // 128
        if need <= fat:
            break
        fat = need
    clusters = part_sectors - RESERVED - 2 * fat
    assert clusters >= 65525, "image too small for FAT32 with one sector per cluster"
    data_start = RESERVED + 2 * fat  # relative to the partition
    img = bytearray(total_sectors * SECTOR)

    def put(lba, data):
        img[lba * SECTOR : lba * SECTOR + len(data)] = data

    fat_table = [0] * (fat * 128)
    fat_table[0] = 0x0FFFFFF8
    fat_table[1] = 0x0FFFFFFF
    next_free = [3]  # cluster 2 is the root directory

    def alloc(n):
        out = list(range(next_free[0], next_free[0] + n))
        next_free[0] += n
        return out

    def chain(cl, end=True):
        for x, y in zip(cl, cl[1:]):
            fat_table[x] = y
        if end:
            fat_table[cl[-1]] = 0x0FFFFFFF

    def cluster_lba(c):
        return PART_START + data_start + (c - 2)

    fat_table[2] = 0x0FFFFFFF
    dir_entries = []

    def short_entry(name11, attr, first, size):
        e = bytearray(32)
        e[0:11] = name11
        e[11] = attr
        e[20:22] = struct.pack("<H", first >> 16)
        e[26:28] = struct.pack("<H", first & 0xFFFF)
        e[28:32] = struct.pack("<I", size)
        return bytes(e)

    def lfn_entries(long_name, short11):
        # The checksum and the UTF-16 pieces of a real long-name sequence.
        s = 0
        for b in short11:
            s = (((s & 1) << 7) + (s >> 1) + b) & 0xFF
        units = [ord(c) for c in long_name] + [0]
        while len(units) % 13:
            units.append(0xFFFF)
        out = []
        n = len(units) // 13
        for i in range(n, 0, -1):
            u = units[(i - 1) * 13 : i * 13]
            e = bytearray(32)
            e[0] = i | (0x40 if i == n else 0)
            e[1:11] = b"".join(struct.pack("<H", x) for x in u[0:5])
            e[11] = 0x0F
            e[13] = s
            e[14:26] = b"".join(struct.pack("<H", x) for x in u[5:11])
            e[28:32] = b"".join(struct.pack("<H", x) for x in u[11:13])
            out.append(bytes(e))
        return out

    noise = Noise(0x9E3779B97F4A7C15)
    # A volume label, a deleted entry, a plain file, a long-named file.
    dir_entries.append(short_entry(b"MILKCARD   ", 0x08, 0, 0))
    gone = bytearray(short_entry(b"MILKLOG TXT", 0x20, 9, 4096))
    gone[0] = 0xE5
    dir_entries.append(bytes(gone))
    readme = alloc(1)
    chain(readme)
    put(cluster_lba(readme[0]), b"hello\n".ljust(SECTOR, b"\0"))
    dir_entries.append(short_entry(b"README  TXT", 0x20, readme[0], 6))
    longc = alloc(2)
    chain(longc)
    for c in longc:
        put(cluster_lba(c), noise.block(SECTOR))
    dir_entries += lfn_entries("Long file name.txt", b"LONGFI~1TXT")
    dir_entries.append(short_entry(b"LONGFI~1TXT", 0x20, longc[0], 1000))
    # The kernel image stand-in: 600 KiB of noise.
    kernel_n = 1200
    kc = alloc(kernel_n)
    chain(kc)
    for c in kc:
        put(cluster_lba(c), noise.block(SECTOR))
    dir_entries.append(short_entry(b"KERNEL8 IMG", 0x20, kc[0], kernel_n * SECTOR))

    # The log file.
    size = a.log_size if a.defect != "tiny" else 3000
    n = -(-size // SECTOR)
    header = b"MILKLOG.TXT: the milkOS log goes here (the kernel overwrites this file in place; nothing below yet).\n"
    if a.defect == "v1":
        # A format 1 log from the first version of the writer: somebody's earlier log, not ours to overwrite.
        header = b"MILKLOG 1 boot=0000abcd\nmilkOS 0.2.0 (build abc1234) for the Raspberry Pi 4\n--------\nboot: entered at EL2\n"
    elif a.defect == "foreign":
        header = b"This file has been used for something else. Do not overwrite it.\n"
    content = (header + b" " * size)[:size]
    if a.fragment:
        log = []
        filler = []
        while len(log) < n:
            log += alloc(min(a.piece, n - len(log)))
            filler += alloc(3)
        chain(filler)
        for c in filler:
            put(cluster_lba(c), noise.block(SECTOR))
        dir_entries.append(short_entry(b"FILLER  BIN", 0x20, filler[0], len(filler) * SECTOR))
    else:
        log = alloc(n)
    chain(log)
    if a.defect == "loop":
        fat_table[log[-1]] = log[2]
    elif a.defect == "short":
        fat_table[log[-3]] = 0x0FFFFFFF
    elif a.defect == "long":
        extra = alloc(2)
        fat_table[log[-1]] = extra[0]
        chain(extra)
    elif a.defect == "range":
        fat_table[log[len(log) // 2]] = clusters + 10
    elif a.defect == "free":
        fat_table[log[len(log) // 2]] = 0
    elif a.defect == "longchain":
        pass
    for i, c in enumerate(log):
        put(cluster_lba(c), content[i * SECTOR : (i + 1) * SECTOR].ljust(SECTOR, b" "))
    if a.defect != "nofile":
        dir_entries.append(short_entry(b"MILKLOG TXT", 0x20, log[0], size))

    # Root directory (cluster 2: 16 entries per sector, one sector).
    assert len(dir_entries) <= 16, len(dir_entries)
    d = b"".join(dir_entries)
    put(cluster_lba(2), d.ljust(SECTOR, b"\0"))

    # Boot sector (and its backup at sector 6), FSInfo not needed.
    bs = bytearray(SECTOR)
    bs[0:3] = b"\xEB\x58\x90"
    bs[3:11] = b"MSWIN4.1"
    bs[11:13] = struct.pack("<H", SECTOR)
    bs[13] = 1
    bs[14:16] = struct.pack("<H", RESERVED)
    bs[16] = 2
    bs[21] = 0xF8
    bs[24:26] = struct.pack("<H", 63)
    bs[26:28] = struct.pack("<H", 255)
    bs[28:32] = struct.pack("<I", PART_START)
    bs[32:36] = struct.pack("<I", part_sectors)
    bs[36:40] = struct.pack("<I", fat)
    bs[44:48] = struct.pack("<I", 2)
    bs[48:50] = struct.pack("<H", 1)
    bs[50:52] = struct.pack("<H", 6)
    bs[64] = 0x80
    bs[66] = 0x29
    bs[67:71] = struct.pack("<I", 0x4D494C4B)
    bs[71:82] = b"MILKCARD   "
    bs[82:90] = b"FAT32   "
    bs[510:512] = b"\x55\xAA"
    put(PART_START, bytes(bs))
    put(PART_START + 6, bytes(bs))
    ft = b"".join(struct.pack("<I", v) for v in fat_table)
    put(PART_START + RESERVED, ft)
    put(PART_START + RESERVED + fat, ft)

    # MBR.
    if a.defect != "nombr":
        mbr = bytearray(SECTOR)
        e = bytearray(16)
        e[0] = 0x80
        e[1:4] = b"\x20\x21\x00"
        e[4] = 0x0C
        e[5:8] = b"\xFE\xFF\xFF"
        e[8:12] = struct.pack("<I", PART_START)
        e[12:16] = struct.pack("<I", part_sectors)
        mbr[446:462] = e
        mbr[510:512] = b"\x55\xAA"
        put(0, bytes(mbr))

    # Extents of the log file, in absolute sectors.
    extents = []
    for c in log[: -(-n // 1)]:
        lba = cluster_lba(c)
        if extents and extents[-1][0] + extents[-1][1] == lba:
            extents[-1][1] += 1
        else:
            extents.append([lba, 1])
    layout = {
        "sector_size": SECTOR,
        "image_sectors": total_sectors,
        "partition_start": PART_START,
        "data_start": PART_START + data_start,
        "log_size": size,
        "log_sectors": n,
        "log_extents": extents,
        "defect": a.defect or "",
    }
    return bytes(img), layout


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("out")
    p.add_argument("--size", type=size_arg, default=64 * 1024 * 1024)
    p.add_argument("--log-size", type=size_arg, default=256 * 1024)
    p.add_argument("--piece", type=int, default=8, help="with --fragment: clusters per piece of the log file (default 8)")
    p.add_argument("--fragment", action="store_true")
    p.add_argument("--defect", choices=["loop", "short", "long", "range", "free", "nofile", "nombr", "tiny", "v1", "foreign"])
    p.add_argument("--layout")
    a = p.parse_args()
    if a.size & (a.size - 1):
        sys.exit("pi_fat32: --size must be a power of two (qemu's SD card model wants that)")
    img, layout = build(a)
    with open(a.out, "wb") as f:
        f.write(img)
    if a.layout:
        with open(a.layout, "w") as f:
            json.dump(layout, f)
    print(f"pi_fat32: {a.out}: {a.size >> 20} MiB, log file {layout['log_size']} bytes in {len(layout['log_extents'])} piece(s)")


if __name__ == "__main__":
    main()
