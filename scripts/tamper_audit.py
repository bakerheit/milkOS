#!/usr/bin/env python3
"""Play the attacker: forge a record in the persisted audit log inside a milkfs image,
the careful way, so milkfs itself sees nothing wrong (python3 stdlib only, uses
scripts/mkmilkfs.py for the format).

  tamper_audit.py IMAGE OLD NEW [--path /var/log/audit.log]

Replaces the first occurrence of OLD with NEW (same length) in the file's data, then
recomputes every milkfs checksum on the way up (data block -> extent item in its leaf ->
B+tree node pointers -> superblock), patching the blocks in place. A plain byte flip would
already be caught by milkfs's block checksums (EIO); this one is only caught by the
audit log's hash chain, which is the point. In-place edits also break the checksums of
any snapshot that shares those blocks (mkmilkfs.py check will say so); the live tree is
consistent. For tests on private images only.
"""
import importlib.util
import os
import struct
import sys

here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("mkmilkfs", os.path.join(here, "mkmilkfs.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
BLOCK = m.BLOCK


def node_items_with_offsets(b):
    """[(key, value_offset, value_len_or_ptr)] for a node block, plus its level."""
    level, _, n, _ = struct.unpack_from("<BBHQ", b, 4)
    p, out = 16, []
    for _ in range(n):
        key = struct.unpack_from("<QBQ", b, p)
        if level == 0:
            ln = struct.unpack_from("<H", b, p + 17)[0]
            out.append((key, p + 19, ln))
            p += 19 + ln
        else:
            out.append((key, p + 17, 24))
            p += 41
    return level, out


def main():
    args = sys.argv[1:]
    path = "/var/log/audit.log"
    if "--path" in args:
        i = args.index("--path")
        path = args[i + 1]
        del args[i:i + 2]
    if len(args) != 3:
        sys.exit(__doc__)
    image, old, new = args[0], args[1].encode(), args[2].encode()
    if len(old) != len(new):
        sys.exit("OLD and NEW must have the same length")
    img = m.Image(image)
    rw = open(image, "r+b")
    slots = [(s, m.sb_decode(img.raw(s))) for s in (0, 1)]
    slot, sb = max(((s, x) for s, x in slots if x), key=lambda t: t[1]["gen"])
    tree = m.Tree(img, sb["root"])
    ino = tree.resolve(path)
    data = tree.read(ino)
    at = data.find(old)
    if at < 0:
        sys.exit(f"{old!r} not found in {path}")
    if (at % BLOCK) + len(old) > BLOCK:
        sys.exit("the text spans two blocks; pick another")
    idx = at // BLOCK
    key = (ino, m.K_EXTENT, idx)

    # Descend from the root to the leaf holding `key`, remembering the path.
    trail = []  # [(ptr, block bytes, offset of the entry we followed / patch)]
    ptr = sb["root"]
    while True:
        b = bytearray(img.read(ptr))
        level, ents = node_items_with_offsets(b)
        if level == 0:
            hit = [e for e in ents if e[0] == key]
            if not hit:
                sys.exit("extent item not found")
            trail.append((ptr, b, hit[0][1]))
            break
        child = None
        for k, off, _ in ents:
            if k <= key:
                child = (off, m.ptr_unpack(b, off))
        if child is None:
            sys.exit("key below the first child")
        trail.append((ptr, b, child[0]))
        ptr = child[1]

    # 1. The data block.
    leaf_ptr, leaf, voff = trail[-1]
    dblk, dbirth, dcsum = m.ptr_unpack(leaf, voff)
    blockdata = bytearray(img.read((dblk, dbirth, dcsum)))
    o = at % BLOCK
    blockdata[o:o + len(old)] = new
    rw.seek(dblk * BLOCK)
    rw.write(blockdata)
    newptr = (dblk, dbirth, m.xxh64(bytes(blockdata)))
    print(f"data block {dblk}: {old.decode()!r} -> {new.decode()!r} at file offset {at}")

    # 2. Up the tree: patch the pointer, re-checksum the node, repeat.
    for nptr, nb, off in reversed(trail):
        nb[off:off + 24] = m.ptr_pack(newptr)
        rw.seek(nptr[0] * BLOCK)
        rw.write(nb)
        newptr = (nptr[0], nptr[1], m.xxh64(bytes(nb)))
        print(f"node block {nptr[0]}: checksum {nptr[2]:#018x} -> {newptr[2]:#018x}")

    # 3. The superblock that is in use.
    sbb = bytearray(img.raw(slot))
    sbb[32:56] = m.ptr_pack(newptr)
    struct.pack_into("<Q", sbb, BLOCK - 8, m.xxh64(bytes(sbb[:BLOCK - 8])))
    rw.seek(slot * BLOCK)
    rw.write(sbb)
    rw.close()
    print(f"superblock slot {slot} (gen {sb['gen']}): root re-pointed, checksum rewritten")


if __name__ == "__main__":
    main()
