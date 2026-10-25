#!/usr/bin/env python3
"""A tiny flattened device tree (FDT) writer and reader, and the device trees the Pixel 2 needs.

The Pixel 2's bootloader (ABL) takes a device tree appended to the kernel, picks the one whose
`qcom,msm-id` matches the SoC, applies the overlay from the dtbo partition and patches the
result (`/memory`, `/chosen/bootargs`, the reserved regions it knows) before it starts the kernel
with the patched tree in x0. Two ways to give it a tree (docs/pixel2.md):

  stock    copy a device tree you already have (the one from the phone's own boot image, which
           you extracted yourself) unchanged. The safest first try: it is what the bootloader
           expects. Nothing of it is kept in this repository.
  minimal  build a small tree from scratch: the root properties ABL matches on, `/memory`,
           `/chosen`, the CPUs, PSCI, the reserved regions of the phone (their addresses are
           facts about the hardware), the GIC and the display controller, and a `/soc` for them.
           Whether ABL accepts such a tree (and the dtbo overlay on it) is exactly what the first
           experiment finds out.

Usage (python3 -I scripts/dtb_build.py ...):
  minimal -o OUT [--model STR] [--msm-id A:B]    build the small tree
  stock   -i IN -o OUT                           copy IN unchanged (checks it is a tree with the right msm-id)
  dump    FILE                                   print a tree (nodes, properties, the reservation block)
  dtbo-list FILE                                 list the entries of a dtbo partition image (id, rev, custom)
  virt-test -o OUT [--no-splash]                 the tree of the qemu virt test (scripts/pixel2-qemu-test.sh)
  golden                                         print the hex of the small test tree (tests compare it with the Rust writer's)

Python 3 standard library only.
"""

import argparse
import struct
import sys

FDT_MAGIC = 0xD00DFEED
BEGIN, END_NODE, PROP, NOP, END = 1, 2, 3, 4, 9
DTBO_MAGIC = 0xD7B7AB1E

# The Pixel 2 (walleye): Qualcomm MSM8998 v2.1. 0x124 is the SoC id, 0x20001 the hardware revision.
WALLEYE_MODEL = "Qualcomm Technologies, Inc. MSM8998 v2.1"
WALLEYE_COMPAT = ["qcom,msm8998-mtp", "qcom,msm8998", "qcom,mtp"]
WALLEYE_MSM_ID = (0x124, 0x20001)

# The fixed regions of the phone's RAM that something else owns (secure world, the modem, the
# DSPs, the display's splash screen). From the device tree Google ships; the kernel must never
# map the ones marked `no-map`.
RESERVED_FIXED = [
    # name, base, size, no-map
    ("removed_regions@85800000", 0x85800000, 0x3700000, True),
    ("spss_region@8ab00000", 0x8AB00000, 0x700000, True),
    ("pil_adsp_region@0x8b200000", 0x8B200000, 0x1A00000, True),
    ("modem_region@8cc00000", 0x8CC00000, 0x7000000, True),
    ("pil_video_region@93c00000", 0x93C00000, 0x500000, True),
    ("pil_mba_region@94100000", 0x94100000, 0x200000, True),
    ("pil_slpi_region@94300000", 0x94300000, 0xF00000, True),
    ("pil_ipa_gpu_region@95200000", 0x95200000, 0x100000, True),
]
SPLASH = ("splash_region@9d600000", 0x9D600000, 0x2400000)


# ---- the writer -----------------------------------------------------------------------


def cells(*v):
    return b"".join(struct.pack(">I", x & 0xFFFFFFFF) for x in v)


def cells64(*v):
    """64-bit values as two cells each."""
    return b"".join(struct.pack(">Q", x) for x in v)


def strs(*s):
    return b"".join(x.encode() + b"\0" for x in s)


class Node:
    def __init__(self, name):
        self.name = name
        self.props = []
        self.children = []

    def prop(self, name, data=b""):
        self.props.append((name, data))
        return self

    def child(self, name):
        n = Node(name)
        self.children.append(n)
        return n


def write_fdt(root, reserve=()):
    """The blob for `root`, laid out like dtc: header, reservation block, structure, strings."""
    struct_b = bytearray()
    strings = bytearray()
    offs = {}

    def pad():
        while len(struct_b) % 4:
            struct_b.append(0)

    def emit(n):
        struct_b.extend(struct.pack(">I", BEGIN))
        struct_b.extend(n.name.encode() + b"\0")
        pad()
        for name, data in n.props:
            if name not in offs:
                offs[name] = len(strings)
                strings.extend(name.encode() + b"\0")
            struct_b.extend(struct.pack(">III", PROP, len(data), offs[name]))
            struct_b.extend(data)
            pad()
        for c in n.children:
            emit(c)
        struct_b.extend(struct.pack(">I", END_NODE))

    emit(root)
    struct_b.extend(struct.pack(">I", END))
    rsv = b"".join(struct.pack(">QQ", a, s) for a, s in reserve) + b"\0" * 16
    off_rsv = 40
    off_struct = off_rsv + len(rsv)
    off_strings = off_struct + len(struct_b)
    total = off_strings + len(strings)
    header = struct.pack(
        ">10I", FDT_MAGIC, total, off_struct, off_strings, off_rsv, 17, 16, 0, len(strings), len(struct_b)
    )
    return header + rsv + bytes(struct_b) + bytes(strings)


# ---- the reader -------------------------------------------------------------------------


def parse_fdt(b, off=0):
    """-> (total_size, reservations, nodes) with nodes = {path: {prop: bytes}} in tree order."""
    (magic, total, off_struct, off_strings, off_rsv, _ver, _last, _cpu, sz_strings, sz_struct) = struct.unpack_from(
        ">10I", b, off
    )
    if magic != FDT_MAGIC:
        raise ValueError("not a device tree (bad magic)")
    if off_struct + sz_struct > total or off_strings + sz_strings > total:
        raise ValueError("device tree blocks run past the end")
    rsv = []
    p = off + off_rsv
    while True:
        a, s = struct.unpack_from(">QQ", b, p)
        p += 16
        if a == 0 and s == 0:
            break
        rsv.append((a, s))
    strs_b = b[off + off_strings : off + off_strings + sz_strings]

    def cstr(buf, at):
        return buf[at : buf.index(b"\0", at)].decode("latin1")

    path = []
    nodes = {}
    p = off + off_struct
    end = p + sz_struct
    while p < end:
        (tok,) = struct.unpack_from(">I", b, p)
        p += 4
        if tok == BEGIN:
            name = cstr(b, p)
            p += (len(name) + 1 + 3) & ~3
            path.append(name)
            nodes.setdefault("/".join(path) or "/", {})
        elif tok == END_NODE:
            path.pop()
        elif tok == PROP:
            ln, noff = struct.unpack_from(">II", b, p)
            p += 8
            nodes["/".join(path) or "/"][cstr(strs_b, noff)] = bytes(b[p : p + ln])
            p += (ln + 3) & ~3
        elif tok == NOP:
            pass
        elif tok == END:
            break
        else:
            raise ValueError(f"bad token {tok} at {p - 4}")
    return total, rsv, nodes


# ---- the trees --------------------------------------------------------------------------


def small_tree():
    """The tree the Rust writer's test builds too (rpikit/src/fdtwrite.rs); same bytes."""
    r = Node("")
    r.prop("#address-cells", cells(2)).prop("#size-cells", cells(2))
    r.prop("model", strs("test")).prop("compatible", strs("a,b", "c,d"))
    r.prop("qcom,msm-id", cells(0x124, 0x20001))
    m = r.child("memory")
    m.prop("device_type", strs("memory")).prop("reg", cells(0, 0x80000000, 0, 0x40000000))
    r.child("chosen").prop("flag")
    return write_fdt(r, [(0x1000, 0x2000)])


def minimal_tree(model=WALLEYE_MODEL, msm_id=WALLEYE_MSM_ID, bootargs=""):
    r = Node("")
    r.prop("#address-cells", cells(2)).prop("#size-cells", cells(2))
    r.prop("model", strs(model)).prop("compatible", strs(*WALLEYE_COMPAT))
    r.prop("qcom,msm-id", cells(*msm_id)).prop("interrupt-parent", cells(1))
    ch = r.child("chosen")
    ch.prop("bootargs", strs(bootargs))
    # ABL fills in the real banks; a zero-length bank is what the stock tree has too.
    mem = r.child("memory")
    mem.prop("device_type", strs("memory")).prop("reg", cells(0, 0, 0, 0))
    cpus = r.child("cpus")
    cpus.prop("#address-cells", cells(2)).prop("#size-cells", cells(0))
    for mpidr in (0, 1, 2, 3, 0x100, 0x101, 0x102, 0x103):
        c = cpus.child(f"cpu@{mpidr:x}")
        c.prop("device_type", strs("cpu")).prop("compatible", strs("arm,armv8"))
        c.prop("reg", cells(0, mpidr)).prop("enable-method", strs("psci"))
    ps = r.child("psci")
    ps.prop("compatible", strs("arm,psci-1.0")).prop("method", strs("smc"))
    rm = r.child("reserved-memory")
    rm.prop("#address-cells", cells(2)).prop("#size-cells", cells(2)).prop("ranges")
    for name, base, size, nomap in RESERVED_FIXED:
        n = rm.child(name)
        n.prop("compatible", strs("removed-dma-pool"))
        if nomap:
            n.prop("no-map")
        n.prop("reg", cells64(base, size))
    name, base, size = SPLASH
    sp = rm.child(name)
    sp.prop("reg", cells64(base, size)).prop("label", strs("cont_splash_mem"))
    soc = r.child("soc")
    soc.prop("#address-cells", cells(1)).prop("#size-cells", cells(1))
    soc.prop("ranges", cells(0, 0, 0xFFFFFFFF)).prop("compatible", strs("simple-bus"))
    gic = soc.child("interrupt-controller@17a00000")
    gic.prop("compatible", strs("arm,gic-v3"))
    gic.prop("reg", cells(0x17A00000, 0x10000, 0x17B00000, 0x100000))
    gic.prop("#interrupt-cells", cells(3)).prop("interrupt-controller")
    gic.prop("#redistributor-regions", cells(1)).prop("redistributor-stride", cells(0, 0x20000))
    gic.prop("phandle", cells(1))
    tm = soc.child("timer")
    tm.prop("compatible", strs("arm,armv8-timer"))
    tm.prop("interrupts", cells(1, 1, 0xF08, 1, 2, 0xF08, 1, 3, 0xF08, 1, 0, 0xF08))
    mdp = soc.child("qcom,mdss_mdp@c900000")
    mdp.prop("compatible", strs("qcom,mdss_mdp"))
    mdp.prop("reg", cells(0xC900000, 0x90000, 0xC9B0000, 0x1040))
    mdp.prop("reg-names", strs("mdp_phys", "vbif_phys"))
    mdp.prop("qcom,mdss-mdp-reg-offset", cells(0x1000))
    mdp.prop("qcom,mdss-ctl-off", cells(0x2000, 0x2200, 0x2400, 0x2600, 0x2800))
    return write_fdt(r)


def virt_test_tree(splash=(0x50000000, 0x2400000), reserved=((0x41000000, 0x100000), (0x42000000, 0x200000)), bootargs=""):
    """A tree for the qemu `virt` test of the Pixel 2 code (scripts/pixel2-qemu-test.sh): qemu's
    addresses for the interrupt controller, RAM-sized reserved regions like the phone's (`no-map`,
    with an address) and a splash region. qemu fills in `/memory` and `/chosen/bootargs` itself."""
    r = Node("")
    r.prop("#address-cells", cells(2)).prop("#size-cells", cells(2))
    r.prop("model", strs("milkOS Pixel 2 test tree")).prop("compatible", strs("linux,dummy-virt"))
    r.prop("interrupt-parent", cells(1))
    r.child("chosen").prop("bootargs", strs(bootargs))
    r.child("memory").prop("device_type", strs("memory")).prop("reg", cells(0, 0x40000000, 0, 0x40000000))
    cpus = r.child("cpus")
    cpus.prop("#address-cells", cells(1)).prop("#size-cells", cells(0))
    c = cpus.child("cpu@0")
    c.prop("device_type", strs("cpu")).prop("compatible", strs("arm,arm-v8")).prop("reg", cells(0))
    ps = r.child("psci")
    ps.prop("compatible", strs("arm,psci-1.0")).prop("method", strs("hvc"))
    rm = r.child("reserved-memory")
    rm.prop("#address-cells", cells(2)).prop("#size-cells", cells(2)).prop("ranges")
    for i, (base, size) in enumerate(reserved):
        n = rm.child(f"test_region{i}@{base:x}")
        n.prop("compatible", strs("removed-dma-pool")).prop("no-map").prop("reg", cells64(base, size))
    if splash:
        sp = rm.child(f"splash_region@{splash[0]:x}")
        sp.prop("reg", cells64(*splash))
    # A watchdog "block" for the test of the register reads and the pet: the last virtio-mmio slot, which
    # has no device behind it (reads give the transport's constants, writes are ignored).
    soc = r.child("soc")
    soc.prop("#address-cells", cells(1)).prop("#size-cells", cells(1)).prop("ranges")
    wd = soc.child("qcom,wdt@a003e00")
    wd.prop("compatible", strs("qcom,msm-watchdog")).prop("reg", cells(0x0A003E00, 0x200))
    intc = r.child("intc@8000000")
    intc.prop("compatible", strs("arm,gic-v3")).prop("#interrupt-cells", cells(3)).prop("interrupt-controller")
    intc.prop("#address-cells", cells(2)).prop("#size-cells", cells(2))
    intc.prop("reg", cells64(0x08000000, 0x10000, 0x080A0000, 0xF60000)).prop("phandle", cells(1))
    return write_fdt(r)


def msm_id_of(nodes):
    v = nodes.get("/", {}).get("qcom,msm-id")
    if v is None or len(v) < 8:
        return None
    return struct.unpack_from(">II", v, 0)


# ---- dtbo partition ----------------------------------------------------------------------


def dtbo_entries(b):
    magic, total, hs, es, cnt, eo, ps, ver = struct.unpack_from(">8I", b, 0)
    if magic != DTBO_MAGIC:
        raise ValueError("not a dtbo table (bad magic)")
    out = []
    for i in range(cnt):
        sz, off, id_, rev, c0, c1, c2, c3 = struct.unpack_from(">8I", b, eo + i * es)
        out.append({"index": i, "size": sz, "offset": off, "id": id_, "rev": rev, "custom": (c0, c1, c2, c3)})
    return out


# ---- command line -------------------------------------------------------------------------


def dump(path, out=sys.stdout):
    b = open(path, "rb").read()
    total, rsv, nodes = parse_fdt(b)
    print(f"total size {total}, {len(nodes)} nodes, reservations {[(hex(a), hex(s)) for a, s in rsv]}", file=out)
    for p, props in nodes.items():
        print(p, file=out)
        for k, v in props.items():
            shown = v if len(v) <= 48 else v[:48] + b"..."
            print(f"    {k} = {shown!r}", file=out)


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    m = sub.add_parser("minimal")
    m.add_argument("-o", "--output", required=True)
    m.add_argument("--model", default=WALLEYE_MODEL)
    m.add_argument("--msm-id", default="0x124:0x20001")
    s = sub.add_parser("stock")
    s.add_argument("-i", "--input", required=True)
    s.add_argument("-o", "--output", required=True)
    d = sub.add_parser("dump")
    d.add_argument("file")
    t = sub.add_parser("dtbo-list")
    t.add_argument("file")
    sub.add_parser("golden")
    v = sub.add_parser("virt-test")
    v.add_argument("-o", "--output", required=True)
    v.add_argument("--no-splash", action="store_true")
    a = ap.parse_args(argv)
    if a.cmd == "minimal":
        x, y = (int(v, 0) for v in a.msm_id.split(":"))
        blob = minimal_tree(a.model, (x, y))
        open(a.output, "wb").write(blob)
        print(f"dtb_build: {a.output} ({len(blob)} bytes, qcom,msm-id {x:#x} {y:#x})")
    elif a.cmd == "stock":
        b = open(a.input, "rb").read()
        total, _, nodes = parse_fdt(b)
        if total != len(b):
            print(f"dtb_build: note, the file is {len(b)} bytes but the tree says {total}; copying the tree only")
        ident = msm_id_of(nodes)
        if ident is None:
            print("dtb_build: that tree has no qcom,msm-id, the bootloader will not pick it", file=sys.stderr)
            return 1
        open(a.output, "wb").write(b[:total])
        print(f"dtb_build: {a.output} ({total} bytes, copied unchanged, qcom,msm-id {ident[0]:#x} {ident[1]:#x})")
    elif a.cmd == "dump":
        dump(a.file)
    elif a.cmd == "dtbo-list":
        for e in dtbo_entries(open(a.file, "rb").read()):
            print(
                f"entry {e['index']:2}: id {e['id']:#x} rev {e['rev']:#x} "
                f"custom {' '.join(f'{c:#x}' for c in e['custom'])}  ({e['size']} bytes at {e['offset']})"
            )
    elif a.cmd == "virt-test":
        blob = virt_test_tree(splash=None if a.no_splash else (0x50000000, 0x2400000))
        open(a.output, "wb").write(blob)
        print(f"dtb_build: {a.output} ({len(blob)} bytes, the qemu virt test tree)")
    elif a.cmd == "golden":
        print(small_tree().hex())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
