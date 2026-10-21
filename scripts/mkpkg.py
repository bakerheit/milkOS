#!/usr/bin/env python3
"""milk packages (.milkpkg) and repo indexes, host side (python3 stdlib only).

  mkpkg.py build SPEC [-o OUT.milkpkg] [--root DIR]   build a package from a spec file
  mkpkg.py index REPO_DIR                             write REPO_DIR/index.txt
  mkpkg.py verify PKG                                 check a package (manifest, payload hash)
  mkpkg.py show PKG                                   print its manifest and members
  mkpkg.py index REPO_DIR --key SECRET                ... and sign index.txt + every package
  mkpkg.py keygen NAME OUTDIR [--test]                new Ed25519 key: NAME.secret, NAME.pub
  mkpkg.py sign SECRET FILE                           write FILE.sig
  mkpkg.py verify-sig PUBFILE FILE                    check FILE.sig
  mkpkg.py selftest                                   Ed25519 RFC 8032 vectors

Signatures (Ed25519, RFC 8032, implemented below in pure python; hashlib only for
SHA-512): FILE.sig is one line `ed25519 <key name> <128 hex chars>` over FILE's bytes.
milk looks the key name up in /etc/milk/keys/<name>.pub (`ed25519 <64 hex chars> <name>`).

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


# ---- Ed25519 (RFC 8032), pure python: written here, hashlib only for SHA-512 -------------
P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
SQRT_M1 = pow(2, (P - 1) // 4, P)


def _inv(x):
    return pow(x, P - 2, P)


def _add(p1, p2):
    """Extended coordinates (X, Y, Z, T), RFC 8032 5.1.4."""
    x1, y1, z1, t1 = p1
    x2, y2, z2, t2 = p2
    a = (y1 - x1) * (y2 - x2) % P
    b = (y1 + x1) * (y2 + x2) % P
    c = t1 * 2 * D * t2 % P
    d = z1 * 2 * z2 % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(k, pt):
    r = (0, 1, 1, 0)
    while k:
        if k & 1:
            r = _add(r, pt)
        pt = _add(pt, pt)
        k >>= 1
    return r


def _recover_x(y, sign):
    if y >= P:
        return None
    x2 = (y * y - 1) * _inv(D * y * y + 1) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * SQRT_M1 % P
    if (x * x - x2) % P:
        return None
    if x & 1 != sign:
        x = P - x
    return x


_GY = 4 * _inv(5) % P
_GX = _recover_x(_GY, 0)
BASE = (_GX, _GY, 1, _GX * _GY % P)


def _encode(pt):
    x, y, z, _ = pt
    zi = _inv(z)
    x, y = x * zi % P, y * zi % P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def _decode(b):
    if len(b) != 32:
        return None
    n = int.from_bytes(b, "little")
    y, sign = n & ((1 << 255) - 1), n >> 255
    x = _recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % P)


def _expand(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def ed25519_public(seed):
    a, _ = _expand(seed)
    return _encode(_mul(a, BASE))


def ed25519_sign(seed, msg):
    a, prefix = _expand(seed)
    pub = _encode(_mul(a, BASE))
    r = int.from_bytes(hashlib.sha512(prefix + msg).digest(), "little") % L
    rb = _encode(_mul(r, BASE))
    k = int.from_bytes(hashlib.sha512(rb + pub + msg).digest(), "little") % L
    return rb + ((r + k * a) % L).to_bytes(32, "little")


def ed25519_verify(pub, msg, sig):
    if len(sig) != 64:
        return False
    a, r = _decode(pub), _decode(sig[:32])
    s = int.from_bytes(sig[32:], "little")
    if a is None or r is None or s >= L:
        return False
    k = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % L
    return _encode(_mul(s, BASE)) == _encode(_add(r, _mul(k, a)))


RFC8032_VECTORS = [  # section 7.1: secret, public, message, signature (hex)
    ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
     "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
     "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
    ("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
     "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72",
     "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
    ("c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
     "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82",
     "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
    ("833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
     "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
     hashlib.sha512(b"abc").hexdigest(),
     "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704"),
]


def ed25519_selftest():
    ok = 0
    for sk, pk, msg, sig in RFC8032_VECTORS:
        sk, pk, msg, sig = (bytes.fromhex(x) for x in (sk, pk, msg, sig))
        bad = bytearray(sig)
        bad[5] ^= 1
        ok += (ed25519_public(sk) == pk) + (ed25519_sign(sk, msg) == sig) \
            + ed25519_verify(pk, msg, sig) + (not ed25519_verify(pk, msg, bytes(bad)))
    return ok, 4 * len(RFC8032_VECTORS)


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


def read_secret(path):
    meta = {}
    for line in open(path, encoding="utf-8"):
        if line.strip() and not line.startswith("#"):
            k, _, v = line.strip().partition(" ")
            meta[k] = v.strip()
    seed = bytes.fromhex(meta["ed25519-seed"])
    if len(seed) != 32:
        sys.exit(f"{path}: bad seed")
    return meta["name"], seed


def sign_file(secret, path):
    name, seed = read_secret(secret)
    sig = ed25519_sign(seed, open(path, "rb").read())
    open(path + ".sig", "w").write(f"ed25519 {name} {sig.hex()}\n")
    return name


def cmd_keygen(name, outdir, test):
    check_name(name)
    seed = os.urandom(32)
    pub = ed25519_public(seed)
    os.makedirs(outdir, exist_ok=True)
    warn = ("# TEST ONLY -- NOT SECRET. Signing key of the milkOS toy repository; anyone can\n"
            "# sign packages with it. Never trust it on a real system.\n") if test else \
        "# Ed25519 signing key for milk packages. Keep this file private.\n"
    sp = os.path.join(outdir, f"{name}.secret")
    with open(sp, "w") as f:
        f.write(warn + f"name {name}\ned25519-seed {seed.hex()}\n")
    os.chmod(sp, 0o600)
    pp = os.path.join(outdir, f"{name}.pub")
    note = "# TEST ONLY key (toy repository).\n" if test else ""
    open(pp, "w").write(note + f"ed25519 {pub.hex()} {name}\n")
    print(f"{sp} (private), {pp} (public: copy to /etc/milk/keys/)")


def cmd_verify_sig(pubfile, path):
    pub = None
    for line in open(pubfile, encoding="utf-8"):
        w = line.split()
        if len(w) >= 2 and w[0] == "ed25519":
            pub = bytes.fromhex(w[1])
    w = open(path + ".sig").read().split()
    ok = pub is not None and len(w) == 3 and w[0] == "ed25519" and \
        ed25519_verify(pub, open(path, "rb").read(), bytes.fromhex(w[2]))
    print(f"{path}: signature {'ok' if ok else 'BAD'} (key {w[1] if len(w) > 1 else '?'})")
    if not ok:
        sys.exit(1)


def cmd_index(repo, key=None):
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
    if key:
        files = ["index.txt"] + sorted(f for f in os.listdir(repo) if f.endswith(".milkpkg"))
        for fn in files:
            name = sign_file(key, os.path.join(repo, fn))
        print(f"{repo}: signed index.txt and {len(files) - 1} packages with key {name}")


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
    elif cmd == "index" and len(rest) == 3 and rest[1] == "--key":
        cmd_index(rest[0], rest[2])
    elif cmd == "keygen" and len(rest) in (2, 3):
        cmd_keygen(rest[0], rest[1], "--test" in rest[2:])
    elif cmd == "sign" and len(rest) == 2:
        print(f"{rest[1]}.sig: key {sign_file(rest[0], rest[1])}")
    elif cmd == "verify-sig" and len(rest) == 2:
        cmd_verify_sig(rest[0], rest[1])
    elif cmd == "selftest":
        ok, n = ed25519_selftest()
        print(f"ed25519 (python): {ok}/{n} RFC 8032 checks")
        sys.exit(0 if ok == n else 1)
    elif cmd == "verify" and len(rest) == 1:
        cmd_verify(rest[0])
    elif cmd == "show" and len(rest) == 1:
        cmd_show(rest[0])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
