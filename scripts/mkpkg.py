#!/usr/bin/env python3
"""milk packages (.milkpkg) and repo indexes, host side (python3 stdlib only).

  mkpkg.py build SPEC [-o OUT.milkpkg] [--root DIR]   build a package from a spec file
  mkpkg.py index REPO_DIR                             write REPO_DIR/index.txt
  mkpkg.py verify PKG                                 check a package (manifest, payload hash)
  mkpkg.py show PKG                                   print its manifest and members

A .milkpkg is a ustar tar: first member `manifest`, then the payload files under their
install paths (relative to /Applications/<name>/). Deterministic: mtime 0, uid/gid 0,
sorted payload, so the same inputs give the same bytes (and sha256).

manifest (text, `key value` per line):
    format 1
    name fortune
    version 1.0.0
    description Milk-themed quotes, one at a time
    exec bin/fortune                       (optional: what /bin/<name> links to)
    dock Fortune #EBB650                   (optional: dock label and icon colour)
    files bin/fortune share/fortune/quotes.txt
    payload-sha256 <hex>

payload-sha256 = SHA-256 over, for each path in `files` order:
    path bytes, 0x00, u64 little-endian size, file bytes
(the milk tool recomputes it after extracting, with the same SHA-256 code as `audit`).

Spec file (packages/<name>/package.txt), paths relative to --root (default: repo root):
    name fortune
    version 1.0.0
    description Milk-themed quotes, one at a time
    exec bin/fortune
    dock Fortune #EBB650
    file bin/fortune = target/packages/aarch64-unknown-none-softfloat/release/fortune
    file share/fortune/quotes.txt = packages/fortune/quotes.txt

index.txt: `# milk repo index v1`, then one line per package:
    <name> <version> <sha256 of the .milkpkg> <size> <file name> <description...>
"""
import hashlib
import io
import os
import struct
import sys
import tarfile

KEYS_ONE = ("name", "version", "description", "exec", "dock")


def payload_hash(files):
    """files: [(path, bytes)] in manifest order."""
    h = hashlib.sha256()
    for path, data in files:
        h.update(path.encode() + b"\0" + struct.pack("<Q", len(data)) + data)
    return h.hexdigest()


def parse_spec(path, root):
    meta, files = {}, []
    for n, raw in enumerate(open(path, encoding="utf-8"), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, _, val = line.partition(" ")
        val = val.strip()
        if key == "file":
            dest, eq, src = val.partition("=")
            if not eq:
                sys.exit(f"{path}:{n}: want 'file DEST = SRC'")
            dest, src = dest.strip(), src.strip()
            files.append((dest, os.path.join(root, src)))
        elif key in KEYS_ONE:
            meta[key] = val
        else:
            sys.exit(f"{path}:{n}: unknown key {key!r}")
    for k in ("name", "version", "description"):
        if k not in meta:
            sys.exit(f"{path}: missing {k}")
    check_name(meta["name"])
    for dest, _ in files:
        if dest.startswith("/") or ".." in dest.split("/") or " " in dest:
            sys.exit(f"{path}: bad install path {dest!r}")
    if "exec" in meta and meta["exec"] not in [d for d, _ in files]:
        sys.exit(f"{path}: exec {meta['exec']!r} is not one of the files")
    return meta, sorted(files)


def check_name(name):
    if not name or len(name) > 24 or not all(c.isalnum() or c in "-_" for c in name):
        sys.exit(f"bad package name {name!r} (1-24 of [A-Za-z0-9_-])")


def manifest_text(meta, files):
    lines = ["format 1"]
    for k in KEYS_ONE:
        if k in meta:
            lines.append(f"{k} {meta[k]}")
    lines.append("files " + " ".join(p for p, _ in files))
    lines.append("payload-sha256 " + payload_hash(files))
    return "\n".join(lines) + "\n"


def add(tar, name, data, mode):
    ti = tarfile.TarInfo(name)
    ti.size = len(data)
    ti.mode = mode
    ti.mtime = 0
    ti.uid = ti.gid = 0
    ti.uname = ti.gname = "root"
    tar.addfile(ti, io.BytesIO(data))


def cmd_build(spec, out, root):
    meta, srcs = parse_spec(spec, root)
    files = [(dest, open(src, "rb").read()) for dest, src in srcs]
    man = manifest_text(meta, files).encode()
    out = out or f"{meta['name']}-{meta['version']}.milkpkg"
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        add(tar, "manifest", man, 0o644)
        for dest, data in files:
            exe = dest.startswith("bin/") or dest == meta.get("exec")
            add(tar, dest, data, 0o755 if exe else 0o644)
    open(out, "wb").write(buf.getvalue())
    print(f"{out}: {meta['name']} {meta['version']}, {len(files)} files, "
          f"{len(buf.getvalue())} bytes, sha256 {hashlib.sha256(buf.getvalue()).hexdigest()[:16]}")


def read_pkg(path):
    data = open(path, "rb").read()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as tar:
        members = tar.getmembers()
        if not members or members[0].name != "manifest":
            raise ValueError("first member is not 'manifest'")
        man = tar.extractfile(members[0]).read().decode()
        payload = {m.name: tar.extractfile(m).read() for m in members[1:] if m.isfile()}
    meta = {}
    for line in man.splitlines():
        k, _, v = line.partition(" ")
        meta[k] = v
    return data, meta, payload


def cmd_verify(path):
    data, meta, payload = read_pkg(path)
    files = meta.get("files", "").split()
    missing = [f for f in files if f not in payload]
    if missing:
        sys.exit(f"{path}: missing payload {missing}")
    got = payload_hash([(f, payload[f]) for f in files])
    if got != meta.get("payload-sha256"):
        sys.exit(f"{path}: payload sha256 mismatch ({got[:16]} != {meta.get('payload-sha256', '')[:16]})")
    print(f"{path}: ok ({meta['name']} {meta['version']}, {len(files)} files, payload {got[:16]})")


def cmd_show(path):
    data, meta, payload = read_pkg(path)
    for k, v in meta.items():
        print(f"{k}: {v}")
    for name, b in payload.items():
        print(f"  {len(b):8}  {name}")
    print(f"sha256 {hashlib.sha256(data).hexdigest()}")


def cmd_index(repo):
    lines = ["# milk repo index v1"]
    for fn in sorted(os.listdir(repo)):
        if not fn.endswith(".milkpkg"):
            continue
        p = os.path.join(repo, fn)
        data, meta, _ = read_pkg(p)
        lines.append(f"{meta['name']} {meta['version']} {hashlib.sha256(data).hexdigest()} "
                     f"{len(data)} {fn} {meta.get('description', '')}")
    open(os.path.join(repo, "index.txt"), "w").write("\n".join(lines) + "\n")
    print(f"{repo}/index.txt: {len(lines) - 1} packages")


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd, rest = argv[1], argv[2:]
    if cmd == "build":
        out, root = None, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
        if "-o" in rest:
            i = rest.index("-o")
            out = rest[i + 1]
            del rest[i:i + 2]
        if "--root" in rest:
            i = rest.index("--root")
            root = rest[i + 1]
            del rest[i:i + 2]
        if len(rest) != 1:
            sys.exit(__doc__)
        cmd_build(rest[0], out, root)
    elif cmd == "index" and len(rest) == 1:
        cmd_index(rest[0])
    elif cmd == "verify" and len(rest) == 1:
        cmd_verify(rest[0])
    elif cmd == "show" and len(rest) == 1:
        cmd_show(rest[0])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
