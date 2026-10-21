#!/usr/bin/env python3
"""milkfs v0 host tool (python3 stdlib only). Format: docs/filesystem.md, kernel/src/fs/milkfs.

  mkmilkfs.py mkfs  IMAGE [--size 64M] [--label Milk] [--force]
  mkmilkfs.py check IMAGE [--quiet]      verify superblock, every checksum, namespace
  mkmilkfs.py check IMAGE --repair       fix what is safe: damaged superblock slot, dangling
                                         entries, orphans, lost inodes (-> /lost+found),
                                         link counts, dir parents. Exit 2 = data damage left
  mkmilkfs.py put   IMAGE PATH FILE [--v0]  copy a Mac file into the image (--v0: old-style
                                         one-block extents, for upgrade tests)
  mkmilkfs.py mkfs  ... --v0             make a format-v0 image (upgrade tests)
  mkmilkfs.py corrupt IMAGE KIND         damage a TEST image: orphan dangling nlink parent
                                         sb data node
  mkmilkfs.py ls    IMAGE [PATH] [--snapshot NAME]      (NAME may also be a clone)
  mkmilkfs.py cat   IMAGE PATH [--snapshot NAME]
  mkmilkfs.py xxh64 HEXDATA               (for cross-checking the kernel's checksum)

mkfs refuses an image that already holds milkfs unless --force. check exits 1 on problems.
"""
import os
import struct
import sys
import time

BLOCK = 4096
MAGIC = b"MILKFS01"
VERSION = 1      # written by mkfs; 0 and 1 are read
MIN_VERSION = 0
MAX_SNAPS = 30
K_INODE, K_DIRENT, K_EXTENT, K_SYMLINK, K_EXT, K_INLINE = 1, 2, 3, 4, 5, 6
ROOT_INO = 1
S_IFMT, S_IFDIR, S_IFREG, S_IFLNK = 0o170000, 0o040000, 0o100000, 0o120000
M64 = (1 << 64) - 1

# ---- xxh64 ---------------------------------------------------------------------------
P1, P2, P3, P4, P5 = (0x9E3779B185EBCA87, 0xC2B2AE3D27D4EB4F, 0x165667B19E3779F9,
                      0x85EBCA77C2B2AE63, 0x27D4EB2F165667C5)


def _rotl(x, r):
    return ((x << r) | (x >> (64 - r))) & M64


def _round(acc, inp):
    return (_rotl((acc + inp * P2) & M64, 31) * P1) & M64


def _merge(acc, val):
    return (((acc ^ _round(0, val)) * P1) + P4) & M64


def xxh64(data):
    n, p = len(data), 0
    if n >= 32:
        v = [(P1 + P2) & M64, P2, 0, (-P1) & M64]
        while p + 32 <= n:
            for i in range(4):
                v[i] = _round(v[i], struct.unpack_from("<Q", data, p + i * 8)[0])
            p += 32
        h = (_rotl(v[0], 1) + _rotl(v[1], 7) + _rotl(v[2], 12) + _rotl(v[3], 18)) & M64
        for x in v:
            h = _merge(h, x)
    else:
        h = P5
    h = (h + n) & M64
    while p + 8 <= n:
        h ^= _round(0, struct.unpack_from("<Q", data, p)[0])
        h = (_rotl(h, 27) * P1 + P4) & M64
        p += 8
    if p + 4 <= n:
        h ^= (struct.unpack_from("<I", data, p)[0] * P1) & M64
        h = (_rotl(h, 23) * P2 + P3) & M64
        p += 4
    while p < n:
        h ^= (data[p] * P5) & M64
        h = (_rotl(h, 11) * P1) & M64
        p += 1
    h ^= h >> 33
    h = (h * P2) & M64
    h ^= h >> 29
    h = (h * P3) & M64
    h ^= h >> 32
    return h


assert xxh64(b"") == 0xEF46DB3751D8E999, "xxh64 self-test"
assert xxh64(b"abc") == 0x44BC2CF5AD770999, "xxh64 self-test"


def name_hash(b):
    h = 0xcbf29ce484222325
    for c in b:
        h = ((h ^ c) * 0x100000001b3) & M64
    return h


# ---- structures ----------------------------------------------------------------------
def ptr_pack(p):
    return struct.pack("<QQQ", *p)


def data_ptrs(k, v):
    """[(file block index, (block, birth, csum))] for a data item (v0 K_EXTENT, v1 K_EXT)."""
    if k[1] == K_EXTENT:
        return [(k[2], ptr_unpack(v))]
    if k[1] == K_EXT:
        block, birth, n = struct.unpack_from("<QQI", v)
        return [(k[2] + i, (block + i, birth, struct.unpack_from("<Q", v, 20 + 8 * i)[0])) for i in range(n)]
    return []


def ptr_unpack(b, o=0):
    return struct.unpack_from("<QQQ", b, o)


def cstr(b):
    return b.split(b"\0", 1)[0].decode("utf-8", "replace")


def sb_encode(sb):
    b = bytearray(BLOCK)
    b[0:8] = MAGIC
    struct.pack_into("<IIQQ", b, 8, sb.get("version", VERSION), BLOCK, sb["total"], sb["gen"])
    b[32:56] = ptr_pack(sb["root"])
    b[56:80] = ptr_pack(sb["dead"])
    struct.pack_into("<QQ", b, 80, sb["next_ino"], sb["created"])
    b[96:112] = sb["uuid"]
    lab = sb["label"].encode()[:31]
    b[112:112 + len(lab)] = lab
    struct.pack_into("<I", b, 144, len(sb["snaps"]))
    for i, s in enumerate(sb["snaps"]):
        o = 152 + i * 128
        nm = s["name"].encode()[:47]
        b[o:o + len(nm)] = nm
        struct.pack_into("<QQ", b, o + 48, s["gen"], s["created"])
        b[o + 64:o + 88] = ptr_pack(s["root"])
        b[o + 88:o + 112] = ptr_pack(s["dead"])
        struct.pack_into("<Q", b, o + 112, s["next_ino"])
        struct.pack_into("<II", b, o + 120, 1 if s.get("clone") else 0, s.get("id", 0))
    struct.pack_into("<4I", b, 3992, *sb.get("auto", (0, 0, 0, 0)))
    struct.pack_into("<I", b, 4008, sb.get("flags", 0))
    struct.pack_into("<Q", b, BLOCK - 8, xxh64(bytes(b[:BLOCK - 8])))
    return bytes(b)


def sb_decode(b):
    if b[0:8] != MAGIC or struct.unpack_from("<Q", b, BLOCK - 8)[0] != xxh64(b[:BLOCK - 8]):
        return None
    ver, bs, total, gen = struct.unpack_from("<IIQQ", b, 8)
    if not MIN_VERSION <= ver <= VERSION or bs != BLOCK:
        return None
    n = struct.unpack_from("<I", b, 144)[0]
    if n > MAX_SNAPS:
        return None
    snaps = []
    for i in range(n):
        o = 152 + i * 128
        g, c = struct.unpack_from("<QQ", b, o + 48)
        flags, rid = struct.unpack_from("<II", b, o + 120)
        snaps.append(dict(name=cstr(b[o:o + 48]), gen=g, created=c, root=ptr_unpack(b, o + 64),
                          dead=ptr_unpack(b, o + 88),
                          next_ino=struct.unpack_from("<Q", b, o + 112)[0],
                          clone=bool(flags & 1), id=rid))
    nino, created = struct.unpack_from("<QQ", b, 80)
    return dict(total=total, gen=gen, root=ptr_unpack(b, 32), dead=ptr_unpack(b, 56),
                flags=struct.unpack_from("<I", b, 4008)[0], version=ver,
                auto=struct.unpack_from("<4I", b, 3992),
                next_ino=nino, created=created, uuid=bytes(b[96:112]),
                label=cstr(b[112:144]), snaps=snaps)


def leaf_encode(items, birth):
    b = bytearray(BLOCK)
    b[0:4] = b"MKND"
    struct.pack_into("<BBHQ", b, 4, 0, 0, len(items), birth)
    p = 16
    for (obj, kind, off), v in items:
        struct.pack_into("<QBQH", b, p, obj, kind, off, len(v))
        b[p + 19:p + 19 + len(v)] = v
        p += 19 + len(v)
    assert p <= BLOCK
    return bytes(b)


def node_decode(b):
    if b[0:4] != b"MKND":
        raise ValueError("bad node magic")
    level, _, n, birth = struct.unpack_from("<BBHQ", b, 4)
    p, out = 16, []
    for _ in range(n):
        obj, kind, off = struct.unpack_from("<QBQ", b, p)
        if level == 0:
            ln = struct.unpack_from("<H", b, p + 17)[0]
            out.append(((obj, kind, off), bytes(b[p + 19:p + 19 + ln])))
            p += 19 + ln
        else:
            out.append(((obj, kind, off), ptr_unpack(b, p + 17)))
            p += 41
    return level, birth, out


def inode_encode(mode, nlink, size, parent, t):
    s, ns = int(t), int((t % 1) * 1e9)
    return struct.pack("<IIIIQQ", mode, nlink, 0, 0, size, parent) + struct.pack("<qI", s, ns) * 3


def inode_decode(v):
    mode, nlink, uid, gid, size, parent = struct.unpack_from("<IIIIQQ", v)
    return dict(mode=mode, nlink=nlink, uid=uid, gid=gid, size=size, parent=parent,
                mtime=struct.unpack_from("<qI", v, 44)[0])


def dirents_decode(v):
    out, p = [], 0
    while p < len(v):
        ino, dt, ln = struct.unpack_from("<QBB", v, p)
        out.append((v[p + 10:p + 10 + ln].decode("utf-8", "replace"), ino, dt))
        p += 10 + ln
    return out


# ---- image access --------------------------------------------------------------------
class Image:
    def __init__(self, path):
        self.f = open(path, "rb")
        self.size = os.fstat(self.f.fileno()).st_size

    def raw(self, blk):
        self.f.seek(blk * BLOCK)
        b = self.f.read(BLOCK)
        return b + bytes(BLOCK - len(b))

    def read(self, p):
        blk, birth, csum = p
        b = self.raw(blk)
        if xxh64(b) != csum:
            raise IOError(f"checksum mismatch in block {blk} (birth {birth})")
        return b

    def superblock(self):
        best = None
        for slot in (0, 1):
            sb = sb_decode(self.raw(slot))
            if sb and (best is None or sb["gen"] > best["gen"]):
                best = sb
        return best

    def items(self, p, nodes=None):
        """All leaf items of the tree at p, in key order. nodes: list of node ptrs seen."""
        if p[0] == 0:
            return
        level, birth, ents = node_decode(self.read(p))
        if nodes is not None:
            nodes.append(p)
        if level == 0:
            yield from ents
        else:
            for _, child in ents:
                yield from self.items(child, nodes)

    def deadlist(self, p):
        entries, chain = [], []
        while p[0] != 0:
            b = self.read(p)
            if b[0:4] != b"MKDL":
                raise ValueError("bad dead-list magic")
            n = struct.unpack_from("<I", b, 4)[0]
            chain.append(p)
            entries += [struct.unpack_from("<QQ", b, 32 + i * 16) for i in range(n)]
            p = ptr_unpack(b, 8)
        return entries, chain


class Tree:
    def __init__(self, img, root):
        self.img = img
        self.items = dict(img.items(root))

    def inode(self, ino):
        v = self.items.get((ino, K_INODE, 0))
        return inode_decode(v) if v else None

    def listdir(self, ino):
        out = []
        for (obj, kind, _), v in sorted(self.items.items()):
            if obj == ino and kind == K_DIRENT:
                out += dirents_decode(v)
        return out

    def resolve(self, path):
        ino = ROOT_INO
        for part in [p for p in path.split("/") if p]:
            m = [e for e in self.listdir(ino) if e[0] == part]
            if not m:
                raise FileNotFoundError(path)
            ino = m[0][1]
        return ino

    def read(self, ino):
        i = self.inode(ino)
        inline = self.items.get((ino, K_INLINE, 0))
        if inline is not None:
            return bytes(inline[:i["size"]]).ljust(i["size"], b"\0")
        blocks = {}
        for k, v in self.items.items():
            if k[0] == ino:
                for idx, p in data_ptrs(k, v):
                    blocks[idx] = p
        data = bytearray()
        for idx in range((i["size"] + BLOCK - 1) // BLOCK):
            data += self.img.read(blocks[idx]) if idx in blocks else bytes(BLOCK)
        return bytes(data[:i["size"]])


def tree_for(img, sb, snapshot):
    if snapshot is None:
        return Tree(img, sb["root"])
    for s in sb["snaps"]:
        if s["name"] == snapshot:
            return Tree(img, s["root"])
    sys.exit(f"no snapshot {snapshot!r}")


# ---- commands ------------------------------------------------------------------------
def parse_size(s):
    mult = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30}
    return int(s[:-1]) * mult[s[-1].upper()] if s[-1].upper() in mult else int(s)


def cmd_mkfs(path, size, label, force, version=None):
    if os.path.exists(path):
        img = Image(path)
        if img.superblock() and not force:
            sys.exit(f"{path} already holds milkfs (use --force to wipe it)")
        if size is None:
            size = img.size
        img.f.close()
    size = size or 64 << 20
    total = size // BLOCK
    if total < 16:
        sys.exit("image too small")
    now = time.time()
    leaf = leaf_encode([((ROOT_INO, K_INODE, 0), inode_encode(S_IFDIR | 0o755, 2, 0, ROOT_INO, now))], 1)
    sb = dict(total=total, gen=1, root=(2, 1, xxh64(leaf)), dead=(0, 0, 0), next_ino=2,
              version=VERSION if version is None else version,
              created=int(now), uuid=os.urandom(16), label=label, snaps=[])
    mode = "r+b" if os.path.exists(path) else "w+b"
    with open(path, mode) as f:
        f.truncate(size)
        f.seek(0)
        f.write(bytes(BLOCK))          # slot 0: no superblock
        f.write(sb_encode(sb))         # slot 1: generation 1
        f.write(leaf)                  # block 2: root leaf
        f.flush()
        os.fsync(f.fileno())
    print(f"{path}: milkfs v{sb['version']}, {total} blocks of 4 KiB, label {label!r}")


def dirents_encode(lst):
    return b"".join(struct.pack("<QBB", ino, dt, len(n.encode())) + n.encode() for n, ino, dt in lst)


def internal_encode(level, entries, birth):
    b = bytearray(BLOCK)
    b[0:4] = b"MKND"
    struct.pack_into("<BBHQ", b, 4, level, 0, len(entries), birth)
    p = 16
    for (obj, kind, off), ptr in entries:
        struct.pack_into("<QBQ", b, p, obj, kind, off)
        b[p + 17:p + 41] = ptr_pack(ptr)
        p += 41
    return bytes(b)


def all_used(img, sb):
    """Every block the superblock reaches (trees, data, dead-list chains, slots)."""
    used = {0, 1}
    for root in [sb["root"]] + [s["root"] for s in sb["snaps"]]:
        nodes = []
        for k, v in img.items(root, nodes):
            used.update(p[0] for _, p in data_ptrs(k, v))
        used.update(p[0] for p in nodes)
    for head in [sb["dead"]] + [s["dead"] for s in sb["snaps"]]:
        used.update(p[0] for p in img.deadlist(head)[1])
    return used


class Writer:
    """Writes new blocks into blocks the current superblock doesn't reach."""

    def __init__(self, path, img, sb):
        self.f = open(path, "r+b")
        used = all_used(img, sb)
        self.free = (b for b in range(2, sb["total"]) if b not in used)

    def put(self, data, birth):
        blk = next(self.free)
        self.f.seek(blk * BLOCK)
        self.f.write(data)
        return (blk, birth, xxh64(data))

    def tree(self, items, birth):
        """Bulk-load a B+tree from {key: value}; returns the root pointer."""
        leaves, cur, size = [], [], 16
        for k, v in sorted(items.items()):
            need = 19 + len(v)
            if cur and size + need > BLOCK:
                leaves.append(cur)
                cur, size = [], 16
            cur.append((k, v))
            size += need
        leaves.append(cur)
        level = [(lv[0][0] if lv else (0, 0, 0), self.put(leaf_encode(lv, birth), birth)) for lv in leaves]
        n = 1
        while len(level) > 1:
            level = [(level[i][0], self.put(internal_encode(n, level[i:i + 99], birth), birth))
                     for i in range(0, len(level), 99)]
            n += 1
        return level[0][1]

    def commit(self, sb, new_root=None):
        """New generation: the superblock write (after an fsync) is the commit point."""
        sb2 = dict(sb, snaps=[dict(x) for x in sb["snaps"]], gen=sb["gen"] + 1)
        if new_root is not None and new_root != sb["root"]:
            sb2["root"] = new_root
            if sb2["snaps"]:
                # The live tree was rewritten outside the dead-list bookkeeping: switch
                # the volume to exact sweeps (like a promote does).
                sb2["flags"] = sb2.get("flags", 0) | 1
                sb2["dead"] = (0, 0, 0)
                for x in sb2["snaps"]:
                    x["dead"] = (0, 0, 0)
        self.f.flush()
        os.fsync(self.f.fileno())
        self.f.seek((sb2["gen"] % 2) * BLOCK)
        self.f.write(sb_encode(sb2))
        self.f.flush()
        os.fsync(self.f.fileno())
        self.f.close()
        return sb2


def analyse(img, sb, live):
    """Kernel-equivalent namespace check of the live items. Returns (problems, plan)."""
    problems, plan = [], dict(dangling=[], reclaim=[], lost=[], nlink=[], parent=[])
    inodes = {k[0]: inode_decode(v) for k, v in live.items() if k[1] == K_INODE}
    refs, subdirs = {}, {}
    for (obj, kind, _), v in live.items():
        if kind != K_DIRENT:
            continue
        for name, ino, dt in dirents_decode(v):
            i = inodes.get(ino)
            if i is None:
                problems.append(f"entry {obj}/{name} -> missing inode {ino}")
                plan["dangling"].append((obj, name))
                continue
            refs.setdefault(ino, []).append(obj)
            if i["mode"] & S_IFMT == S_IFDIR:
                subdirs[obj] = subdirs.get(obj, 0) + 1
    for ino, i in inodes.items():
        is_dir = i["mode"] & S_IFMT == S_IFDIR
        dirs = refs.get(ino)
        if ino != ROOT_INO and not dirs:
            if i["nlink"] == 0:
                problems.append(f"inode {ino} has no links and no entry (orphan)")
                plan["reclaim"].append(ino)
            else:
                problems.append(f"inode {ino} ({i['nlink']} links) is in no directory")
                plan["lost"].append(ino)
            continue
        want = 2 + subdirs.get(ino, 0) if is_dir else len(dirs)
        if i["nlink"] != want:
            problems.append(f"inode {ino} nlink {i['nlink']} != {want}")
            plan["nlink"].append((ino, want))
        if is_dir and ino != ROOT_INO and i["parent"] != dirs[0]:
            problems.append(f"dir {ino} parent {i['parent']} != {dirs[0]}")
            plan["parent"].append((ino, dirs[0]))
    if ROOT_INO not in inodes:
        problems.append("no root directory")
    return problems, plan


def set_inode_field(items, ino, off, fmt, value):
    k = (ino, K_INODE, 0)
    v = bytearray(items[k])
    struct.pack_into(fmt, v, off, value)
    items[k] = bytes(v)


def bucket_edit(items, dir_ino, name, fn):
    k = (dir_ino, K_DIRENT, name_hash(name.encode()))
    lst = fn(dirents_decode(items[k]) if k in items else [])
    if lst:
        items[k] = dirents_encode(lst)
    else:
        items.pop(k, None)


def apply_repairs(items, sb, plan, log):
    for d, name in plan["dangling"]:
        bucket_edit(items, d, name, lambda l: [e for e in l if e[0] != name])
        log.append(f"removed entry {name} (missing inode)")
    for ino in plan["reclaim"]:
        for k in [k for k in items if k[0] == ino and k[1] in (K_INODE, K_EXTENT, K_SYMLINK, K_EXT, K_INLINE)]:
            del items[k]
        log.append(f"deleted orphan inode {ino}")
    for ino, n in plan["nlink"]:
        set_inode_field(items, ino, 4, "<I", n)
        log.append(f"inode {ino}: link count set to {n}")
    for ino, p in plan["parent"]:
        set_inode_field(items, ino, 24, "<Q", p)
        log.append(f"directory {ino}: parent set to {p}")
    if plan["lost"]:
        root_entries = [e for (o, k, _), v in items.items() if o == ROOT_INO and k == K_DIRENT
                        for e in dirents_decode(v)]
        lf = next((ino for n, ino, _ in root_entries if n == "lost+found"), None)
        if lf is None:
            lf = sb["next_ino"]
            sb["next_ino"] += 1
            items[(lf, K_INODE, 0)] = inode_encode(S_IFDIR | 0o700, 2, 0, ROOT_INO, time.time())
            bucket_edit(items, ROOT_INO, "lost+found", lambda l: l + [("lost+found", lf, 4)])
            r = inode_decode(items[(ROOT_INO, K_INODE, 0)])
            set_inode_field(items, ROOT_INO, 4, "<I", r["nlink"] + 1)
        for ino in plan["lost"]:
            i = inode_decode(items[(ino, K_INODE, 0)])
            is_dir = i["mode"] & S_IFMT == S_IFDIR
            name = f"#{ino}"
            bucket_edit(items, lf, name, lambda l: l + [(name, ino, 4 if is_dir else 8)])
            if is_dir:
                set_inode_field(items, ino, 24, "<Q", lf)
                n = inode_decode(items[(lf, K_INODE, 0)])["nlink"]
                set_inode_field(items, lf, 4, "<I", n + 1)
            else:
                set_inode_field(items, ino, 4, "<I", 1)
            log.append(f"moved inode {ino} to /lost+found/{name}")


def cmd_check_repair(path):
    """check --repair: fix what is safe to fix, by rewriting the live tree."""
    img = Image(path)
    sb = img.superblock()
    if not sb:
        print(f"{path}: no valid milkfs superblock in either slot: cannot repair")
        return 2
    other = (sb["gen"] + 1) % 2
    o = sb_decode(img.raw(other))
    bad_slot = sb["gen"] > 1 and (o is None or o["gen"] >= sb["gen"])
    try:
        live = dict(img.items(sb["root"]))
    except (IOError, ValueError) as e:
        print(f"cannot repair: live tree unreadable ({e}); recover from a snapshot")
        return 2
    bad_data = []
    for k, v in live.items():
        for idx, p in data_ptrs(k, v):
            try:
                img.read(p)
            except IOError:
                bad_data.append(f"inode {k[0]} data at offset {idx * BLOCK} fails its checksum")
    problems, plan = analyse(img, sb, live)
    log = []
    if problems or bad_slot:
        w = Writer(path, img, sb)
        sb = dict(sb)
        if any(plan.values()):
            apply_repairs(live, sb, plan, log)
            new_root = w.tree(live, sb["gen"] + 1)
        else:
            new_root = None
        sb2 = w.commit(sb, new_root)
        if bad_slot:
            log.append(f"superblock slot {other} rewritten (generation {sb2['gen']})")
    for p in problems:
        print("  problem:", p)
    for r in log:
        print("  repaired:", r)
    for b in bad_data:
        print("  cannot repair:", b)
    print("repair:", "nothing to do" if not log else f"{len(log)} repair(s)",
          "(data damage remains)" if bad_data else "")
    return 2 if bad_data else 0


def cmd_put(path, dest, src, v0=False):
    """Write host file SRC into the image at DEST (parent directories created). Data as v1
    extents (or inline when small), or as v0 one-block extents with --v0."""
    img = Image(path)
    sb = img.superblock()
    live = dict(img.items(sb["root"]))
    data = open(src, "rb").read()
    now = time.time()
    sb = dict(sb)
    if not v0:
        sb["version"] = max(sb.get("version", 0), 1)  # v1 items: the image is v1 now
    w = Writer(path, img, sb)
    gen = sb["gen"] + 1
    parts = [p for p in dest.split("/") if p]
    d = ROOT_INO
    for name in parts[:-1]:
        hit = [ino for (o, k, _), v in live.items() if o == d and k == K_DIRENT
               for n, ino, _ in dirents_decode(v) if n == name]
        if hit:
            d = hit[0]
            continue
        ino = sb["next_ino"]
        sb["next_ino"] += 1
        live[(ino, K_INODE, 0)] = inode_encode(S_IFDIR | 0o755, 2, 0, d, now)
        bucket_edit(live, d, name, lambda l, n=name, i=ino: l + [(n, i, 4)])
        pi = inode_decode(live[(d, K_INODE, 0)])
        set_inode_field(live, d, 4, "<I", pi["nlink"] + 1)
        d = ino
    name = parts[-1]
    if any(n == name for (o, k, _), v in live.items() if o == d and k == K_DIRENT
           for n, _, _ in dirents_decode(v)):
        sys.exit(f"{dest} exists")
    ino = sb["next_ino"]
    sb["next_ino"] += 1
    live[(ino, K_INODE, 0)] = inode_encode(S_IFREG | 0o644, 1, len(data), d, now)
    bucket_edit(live, d, name, lambda l: l + [(name, ino, 8)])
    if len(data) <= 1024 and not v0:
        live[(ino, K_INLINE, 0)] = data
    else:
        blocks = [data[i:i + BLOCK].ljust(BLOCK, b"\0") for i in range(0, len(data), BLOCK)]
        if v0:
            for i, blk in enumerate(blocks):
                live[(ino, K_EXTENT, i)] = ptr_pack(w.put(blk, gen))
        else:
            for i in range(0, len(blocks), 128):
                ptrs = [w.put(b, gen) for b in blocks[i:i + 128]]
                # put() hands out increasing free blocks; split where they aren't consecutive
                run = [ptrs[0]]
                start = i
                for j, p in enumerate(ptrs[1:], 1):
                    if p[0] == run[-1][0] + 1:
                        run.append(p)
                        continue
                    live[(ino, K_EXT, start)] = struct.pack("<QQI", run[0][0], gen, len(run)) + b"".join(struct.pack("<Q", x[2]) for x in run)
                    run, start = [p], i + j
                live[(ino, K_EXT, start)] = struct.pack("<QQI", run[0][0], gen, len(run)) + b"".join(struct.pack("<Q", x[2]) for x in run)
    w.commit(sb, w.tree(live, gen))
    print(f"{path}: {dest} ({len(data)} bytes{', v0 extents' if v0 else ''})")
    return 0


def cmd_corrupt(path, kind):
    """Damage a (private, test) image on purpose: orphan, dangling, nlink, parent, sb,
    data, node."""
    img = Image(path)
    sb = img.superblock()
    live = dict(img.items(sb["root"]))
    files = [k[0] for k, v in live.items() if k[1] == K_INODE and inode_decode(v)["mode"] & S_IFMT == S_IFREG]
    dirs = [k[0] for k, v in live.items() if k[1] == K_INODE and k[0] != ROOT_INO
            and inode_decode(v)["mode"] & S_IFMT == S_IFDIR]
    f = open(path, "r+b")
    if kind == "sb":
        f.seek(((sb["gen"] + 1) % 2) * BLOCK)
        f.write(b"\xab" * BLOCK)
    elif kind == "data":
        p = next(data_ptrs(k, v)[0][1] for k, v in live.items() if k[1] in (K_EXTENT, K_EXT))
        f.seek(p[0] * BLOCK)
        f.write(bytes(BLOCK))
    elif kind == "node":
        nodes = []
        list(img.items(sb["root"], nodes))
        f.seek(nodes[-1][0] * BLOCK)  # a leaf
        f.write(bytes(BLOCK))
    else:
        f.close()
        if kind == "orphan":
            ino = sb["next_ino"]
            sb = dict(sb, next_ino=ino + 1)
            live[(ino, K_INODE, 0)] = inode_encode(S_IFREG | 0o644, 1, 0, ROOT_INO, time.time())
        elif kind == "dangling":
            bucket_edit(live, ROOT_INO, "ghost", lambda l: l + [("ghost", 999999, 8)])
        elif kind == "nlink":
            set_inode_field(live, files[0], 4, "<I", 9)
        elif kind == "parent":
            set_inode_field(live, dirs[0], 24, "<Q", 999999)
        else:
            sys.exit(f"unknown damage {kind!r}")
        w = Writer(path, img, sb)
        w.commit(sb, w.tree(live, sb["gen"] + 1))
        print(f"{path}: corrupted ({kind})")
        return 0
    f.flush()
    os.fsync(f.fileno())
    f.close()
    print(f"{path}: corrupted ({kind})")
    return 0


def cmd_check(path, quiet):
    img = Image(path)
    problems = []
    sb = img.superblock()
    if not sb:
        print(f"{path}: no valid milkfs superblock")
        return 1
    used = {0, 1}

    def walk(root, what):
        nodes = []
        try:
            items = list(img.items(root, nodes))
        except (IOError, ValueError) as e:
            problems.append(f"{what}: {e}")
            return {}
        for p in nodes:
            used.add(p[0])
        prev = None
        for k, v in items:
            if prev is not None and k <= prev:
                problems.append(f"{what}: keys out of order at {k}")
            prev = k
            for idx, p in data_ptrs(k, v):
                used.add(p[0])
                try:
                    img.read(p)
                except IOError as e:
                    problems.append(f"{what}: inode {k[0]} block {idx}: {e}")
        return dict(items)

    live = walk(sb["root"], "live")
    for s in sb["snaps"]:
        walk(s["root"], f"{'clone' if s['clone'] else 'snapshot'} {s['name']}")
    for name, head in [("live", sb["dead"])] + [(s["name"], s["dead"]) for s in sb["snaps"]]:
        try:
            _, chain = img.deadlist(head)
            used.update(p[0] for p in chain)
        except (IOError, ValueError) as e:
            problems.append(f"dead list {name}: {e}")

    # namespace of the live tree
    inodes = {k[0]: inode_decode(v) for k, v in live.items() if k[1] == K_INODE}
    refs, subdirs = {}, {}
    for (obj, kind, _), v in live.items():
        if kind != K_DIRENT:
            continue
        for name, ino, dt in dirents_decode(v):
            refs[ino] = refs.get(ino, 0) + 1
            i = inodes.get(ino)
            if i is None:
                problems.append(f"entry {obj}/{name} -> missing inode {ino}")
            elif i["mode"] & S_IFMT == S_IFDIR:
                subdirs[obj] = subdirs.get(obj, 0) + 1
                if i["parent"] != obj:
                    problems.append(f"dir {ino} parent {i['parent']} != {obj}")
    orphans = 0
    for ino, i in inodes.items():
        r = refs.get(ino, 0)
        if ino != ROOT_INO and r == 0:
            if i["nlink"] == 0:
                orphans += 1  # unlinked while open: the next mount reclaims it
            else:
                problems.append(f"inode {ino} ({i['nlink']} links) is in no directory")
            continue
        want = 2 + subdirs.get(ino, 0) if i["mode"] & S_IFMT == S_IFDIR else r
        if i["nlink"] != want:
            problems.append(f"inode {ino} nlink {i['nlink']} != {want}")
    if ROOT_INO not in inodes:
        problems.append("no root directory")
    other = (sb["gen"] + 1) % 2
    o = sb_decode(img.raw(other))
    notes = []
    if sb["gen"] > 1 and (o is None or o["gen"] >= sb["gen"]):
        # Not damage: what a commit interrupted mid-superblock-write leaves; the next
        # commit overwrites that slot. --repair rewrites it now.
        notes.append(f"superblock slot {other} is torn (an interrupted commit; the next commit rewrites it)")

    if not quiet or problems:
        print(f"{path}: milkfs v{sb['version']} label {sb['label']!r} generation {sb['gen']}, "
              f"{len(inodes)} inodes, {len(used)}/{sb['total']} blocks used, "
              f"{len(sb['snaps'])} snapshot(s), {orphans} orphan(s)")
        if sb["flags"] & 1:
            print("  history branched (a clone was promoted): no dead lists, sweeps reclaim space")
        for s in sb["snaps"]:
            if s["clone"]:
                print(f"  clone {s['name']} (id {s['id']}) of the snapshot at gen {s['gen']}")
            else:
                print(f"  snapshot {s['name']} gen {s['gen']}")
    for n in notes:
        print("  note:", n)
    for p in problems:
        print("  PROBLEM:", p)
    print("check:", "clean" if not problems else f"{len(problems)} problem(s)")
    return 1 if problems else 0


def cmd_ls(path, p, snapshot):
    img = Image(path)
    t = tree_for(img, img.superblock(), snapshot)
    ino = t.resolve(p)
    for name, cino, dt in sorted(t.listdir(ino)):
        i = t.inode(cino) or {}
        kind = {S_IFDIR: "d", S_IFLNK: "l"}.get(i.get("mode", 0) & S_IFMT, "-")
        print(f"{kind}{i.get('mode', 0) & 0o777:o} {i.get('nlink', 0):3} {i.get('size', 0):9}  {name}")


def cmd_cat(path, p, snapshot):
    img = Image(path)
    t = tree_for(img, img.superblock(), snapshot)
    sys.stdout.buffer.write(t.read(t.resolve(p)))


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd, args = argv[0], argv[1:]
    opts, pos = {}, []
    i = 0
    while i < len(args):
        a = args[i]
        if a in ("--size", "--label", "--snapshot"):
            opts[a] = args[i + 1]
            i += 2
        elif a.startswith("--"):
            opts[a] = True
            i += 1
        else:
            pos.append(a)
            i += 1
    if cmd == "mkfs":
        cmd_mkfs(pos[0], parse_size(opts["--size"]) if "--size" in opts else None,
                 opts.get("--label", "Milk"), "--force" in opts, 0 if "--v0" in opts else None)
    elif cmd == "check":
        if "--repair" in opts:
            return cmd_check_repair(pos[0])
        return cmd_check(pos[0], "--quiet" in opts)
    elif cmd == "corrupt":
        return cmd_corrupt(pos[0], pos[1])
    elif cmd == "put":
        return cmd_put(pos[0], pos[1], pos[2], "--v0" in opts)
    elif cmd == "ls":
        cmd_ls(pos[0], pos[1] if len(pos) > 1 else "/", opts.get("--snapshot"))
    elif cmd == "cat":
        cmd_cat(pos[0], pos[1], opts.get("--snapshot"))
    elif cmd == "xxh64":
        print(f"{xxh64(bytes.fromhex(pos[0])):#018x}")
    else:
        sys.exit(__doc__)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
