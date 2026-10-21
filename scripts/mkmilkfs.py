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
  mkmilkfs.py mkfs  ... --encrypt [--recovery-out FILE]
                                         an encrypted volume (asks for a passphrase; prints
                                         the recovery key, or writes it to FILE)
  mkmilkfs.py change-passphrase IMAGE    rewrap the volume key under a new passphrase
  mkmilkfs.py selftest-crypto            this tool's crypto against the published vectors
  An image milkOS is encrypting or decrypting in place is read in either form (check
  counts both); commands that write refuse it until milkOS has finished.
  Encrypted images: every command takes --passphrase-file FILE, --recovery KEY or
  $MILKFS_PASSPHRASE (else it asks); new passphrases --new-passphrase-file FILE or
  $MILKFS_NEW_PASSPHRASE.
  mkmilkfs.py mkdir IMAGE PATH [MODE]     make a directory (and its parents), mode octal (755)
  mkmilkfs.py populate IMAGE DIR N        add N files under DIR (100 per subdirectory dNNN;
                                         fNNNNN with .md / .txt / no extension in turn; small
                                         ones inline, every 50th 8 KiB in extents; each text
                                         holds "uniqNNNNN" and one of 7 fruit words, FRUITS[i
                                         % 7]): for mount-time, tree and search tests
  mkmilkfs.py corrupt IMAGE KIND         damage a TEST image: orphan dangling nlink parent
                                         sb smap bref data node
  mkmilkfs.py corrupt IMAGE flip PATH [--snapshot NAME]
                                         flip one bit in the first data block of PATH
  mkmilkfs.py verify-stream FILE         check a `snap send` stream (every record checksum,
                                         counts, chained hash) and describe it
  mkmilkfs.py seal-stream FILE OUT        encrypt a plaintext stream (asks for a passphrase)
  Encrypted streams (verify-stream, receive): --stream-passphrase-file FILE or
  $MILKFS_STREAM_PASSPHRASE (else it asks).
  mkmilkfs.py receive IMAGE FILE [--force]  recreate the stream's snapshot in IMAGE (full:
                                         IMAGE must be empty; incremental: its base must be
                                         there, --force rolls the live tree back to it)
  mkmilkfs.py tree  IMAGE [--snapshot NAME]  every path with type, mode, size, mtime and a
                                         content hash (diff two of these to compare trees)
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
VERSION = 1      # written by mkfs; 0, 1, 2 (reflinks) and 3 (encrypted) are read
MAX_VERSION = 3
CRYPT_VERSION = 3
SB_ENCRYPTED = 2
SB_CONVERTING = 4       # being encrypted (or, with SB_TO_PLAIN, decrypted) in place:
SB_TO_PLAIN = 8         # a block is in either form, its pointer's checksum says which
K_CONVERT, K_REMAP = 9, 10  # (VOL_OBJ, ...) items of a conversion in progress
MIN_VERSION = 0
MAX_SNAPS = 30
K_INODE, K_DIRENT, K_EXTENT, K_SYMLINK, K_EXT, K_INLINE = 1, 2, 3, 4, 5, 6
K_ORPHAN = 7            # (VOL_OBJ, K_ORPHAN, ino): unlinked while open
K_BREF = 8              # (VOL_OBJ, K_BREF, first block) -> len u32 | extra u32 (v2)
VOL_OBJ = (1 << 64) - 1
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


# ---- crypto (mirrors kernel/src/crypto and milkfs/crypt.rs; docs/crypto.md) ------------
# Stdlib only. ChaCha20 uses the `cryptography` package for speed if it is installed (pure
# Python otherwise), scrypt uses hashlib.scrypt or `cryptography` (pure Python is slow).
import hashlib
import hmac as _hmac

KDF_LOG_N, KDF_R, KDF_P = 16, 8, 4
SLOT_PASSPHRASE, SLOT_RECOVERY = 1, 2


def _chacha_block(key, counter, nonce12, rounds_only=False):
    def r(x, n):
        return ((x << n) | (x >> (32 - n))) & 0xffffffff

    s = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574] + list(struct.unpack("<8I", key)) + \
        [counter] + list(struct.unpack("<3I", nonce12))
    if rounds_only:  # HChaCha20: counter+nonce are the 16-byte nonce
        pass
    x = list(s)

    def qr(a, b, c, d):
        x[a] = (x[a] + x[b]) & 0xffffffff
        x[d] = r(x[d] ^ x[a], 16)
        x[c] = (x[c] + x[d]) & 0xffffffff
        x[b] = r(x[b] ^ x[c], 12)
        x[a] = (x[a] + x[b]) & 0xffffffff
        x[d] = r(x[d] ^ x[a], 8)
        x[c] = (x[c] + x[d]) & 0xffffffff
        x[b] = r(x[b] ^ x[c], 7)

    for _ in range(10):
        qr(0, 4, 8, 12); qr(1, 5, 9, 13); qr(2, 6, 10, 14); qr(3, 7, 11, 15)
        qr(0, 5, 10, 15); qr(1, 6, 11, 12); qr(2, 7, 8, 13); qr(3, 4, 9, 14)
    if rounds_only:
        return x
    return struct.pack("<16I", *[(x[i] + s[i]) & 0xffffffff for i in range(16)])


def hchacha20(key, n16):
    c, n12 = struct.unpack_from("<I", n16)[0], n16[4:]
    x = _chacha_block(key, c, n12, rounds_only=True)
    return struct.pack("<8I", *(x[0:4] + x[12:16]))


try:
    from cryptography.hazmat.primitives.ciphers import Cipher as _Cipher, algorithms as _alg

    def chacha20_xor(key, counter, nonce12, data):
        c = _Cipher(_alg.ChaCha20(key, struct.pack("<I", counter) + nonce12), mode=None).encryptor()
        return c.update(bytes(data))
except ImportError:
    def chacha20_xor(key, counter, nonce12, data):
        out = bytearray(data)
        for i in range(0, len(out), 64):
            ks = _chacha_block(key, counter + i // 64, nonce12)
            for j in range(min(64, len(out) - i)):
                out[i + j] ^= ks[j]
        return bytes(out)


def xchacha20_xor(key, nonce24, counter, data):
    return chacha20_xor(hchacha20(key, nonce24[:16]), counter, b"\0\0\0\0" + nonce24[16:], data)


def poly1305(key, msg):
    r = int.from_bytes(key[:16], "little") & 0x0ffffffc0ffffffc0ffffffc0fffffff
    s, p, a = int.from_bytes(key[16:], "little"), (1 << 130) - 5, 0
    for i in range(0, len(msg), 16):
        a = (a + int.from_bytes(msg[i:i + 16] + b"\x01", "little")) * r % p
    return ((a + s) & ((1 << 128) - 1)).to_bytes(16, "little")


def _aead_tag(key, nonce12, aad, ct):
    otk = _chacha_block(key, 0, nonce12)[:32]
    pad = lambda b: b"\0" * ((16 - len(b) % 16) % 16)
    return poly1305(otk, aad + pad(aad) + ct + pad(ct) + struct.pack("<QQ", len(aad), len(ct)))


def xaead_seal(key, nonce24, aad, pt):
    sub, n12 = hchacha20(key, nonce24[:16]), b"\0\0\0\0" + nonce24[16:]
    if _FastAEAD:
        out = _FastAEAD(sub).encrypt(n12, bytes(pt), aad)
        return out[:-16], out[-16:]
    ct = chacha20_xor(sub, 1, n12, pt)
    return ct, _aead_tag(sub, n12, aad, ct)


def xaead_open(key, nonce24, aad, ct, tag):
    sub, n12 = hchacha20(key, nonce24[:16]), b"\0\0\0\0" + nonce24[16:]
    if _FastAEAD:
        try:
            return _FastAEAD(sub).decrypt(n12, bytes(ct) + bytes(tag), aad)
        except Exception:
            return None
    if not _hmac.compare_digest(_aead_tag(sub, n12, aad, ct), tag):
        return None
    return chacha20_xor(sub, 1, n12, ct)


try:  # ChaCha20-Poly1305 (IETF) from `cryptography` when installed: same bytes, faster
    from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305 as _FastAEAD
except ImportError:
    _FastAEAD = None


def hmac256(key, data):
    return _hmac.new(key, data, hashlib.sha256).digest()


def _scrypt_py(pw, salt, n, r, p, dklen):
    def salsa8(b):
        x = list(b)

        def R(a, k):
            return ((a << k) | (a >> (32 - k))) & 0xffffffff
        for _ in range(4):
            for (a, b_, c, d) in ((4, 0, 12, 8), (9, 5, 1, 13), (14, 10, 6, 2), (3, 15, 11, 7)):
                x[a] ^= R((x[b_] + x[c]) & 0xffffffff, 7); x[d] ^= R((x[a] + x[b_]) & 0xffffffff, 9)
                x[c] ^= R((x[d] + x[a]) & 0xffffffff, 13); x[b_] ^= R((x[c] + x[d]) & 0xffffffff, 18)
            for (a, b_, c, d) in ((1, 0, 3, 2), (6, 5, 4, 7), (11, 10, 9, 8), (12, 15, 14, 13)):
                x[a] ^= R((x[b_] + x[c]) & 0xffffffff, 7); x[d] ^= R((x[a] + x[b_]) & 0xffffffff, 9)
                x[c] ^= R((x[d] + x[a]) & 0xffffffff, 13); x[b_] ^= R((x[c] + x[d]) & 0xffffffff, 18)
        return [(x[i] + b[i]) & 0xffffffff for i in range(16)]

    def blockmix(B):
        X, Y = B[-16:], []
        for i in range(2 * r):
            X = salsa8([X[k] ^ B[i * 16 + k] for k in range(16)])
            Y.append(X)
        return [w for i in list(range(0, 2 * r, 2)) + list(range(1, 2 * r, 2)) for w in Y[i]]

    B = hashlib.pbkdf2_hmac("sha256", pw, salt, 1, p * 128 * r)
    out = b""
    for i in range(p):
        X = list(struct.unpack("<%dI" % (32 * r), B[i * 128 * r:(i + 1) * 128 * r]))
        V = []
        for _ in range(n):
            V.append(X)
            X = blockmix(X)
        for _ in range(n):
            j = X[(2 * r - 1) * 16] & (n - 1)
            X = blockmix([a ^ b for a, b in zip(X, V[j])])
        out += struct.pack("<%dI" % (32 * r), *X)
    return hashlib.pbkdf2_hmac("sha256", pw, out, 1, dklen)


def scrypt(pw, salt, log_n, r, p, dklen=32):
    n = 1 << log_n
    if hasattr(hashlib, "scrypt"):
        return hashlib.scrypt(pw, salt=salt, n=n, r=r, p=p, dklen=dklen, maxmem=256 << 20)
    try:
        from cryptography.hazmat.primitives.kdf.scrypt import Scrypt
        return Scrypt(salt=salt, length=dklen, n=n, r=r, p=p).derive(pw)
    except ImportError:
        if n > 1024:
            print("mkmilkfs: no fast scrypt here (pure Python): this takes a while...", file=sys.stderr)
        return _scrypt_py(pw, salt, n, r, p, dklen)


class VolumeKey:
    def __init__(self, vk):
        self.vk = vk
        self.enc = hmac256(vk, b"milkfs v1 enc")
        self.mac = hmac256(vk, b"milkfs v1 mac")

    def tag(self, block, birth, data):
        return struct.unpack("<Q", hmac256(self.mac, b"MKB1" + struct.pack("<QQ", block, birth) + data)[:8])[0]

    def seal(self, block, birth, data):
        t = self.tag(block, birth, data)
        return xchacha20_xor(self.enc, struct.pack("<QQQ", t, block, birth), 0, data), t

    def open(self, block, birth, csum, data):
        pt = xchacha20_xor(self.enc, struct.pack("<QQQ", csum, block, birth), 0, data)
        return pt if _hmac.compare_digest(struct.pack("<Q", self.tag(block, birth, pt)), struct.pack("<Q", csum)) else None

    def sb_mac(self, sb_bytes):
        return struct.unpack("<Q", hmac256(self.mac, b"MKSB" + sb_bytes[:4076])[:8])[0]


def keyring_encode(uuid, slots):
    b = bytearray(BLOCK)
    b[0:4] = b"MKKR"
    struct.pack_into("<I", b, 4, 1)
    b[8:24] = uuid
    struct.pack_into("<I", b, 24, len(slots))
    for i, sl in enumerate(slots):
        o = 32 + i * 128
        b[o:o + 4] = bytes([sl["kind"], sl["log_n"], sl["r"], sl["p"]])
        b[o + 8:o + 24], b[o + 24:o + 48] = sl["salt"], sl["nonce"]
        b[o + 48:o + 80], b[o + 80:o + 96] = sl["wrapped"], sl["tag"]
    return bytes(b)


def keyring_decode(b):
    if b[0:4] != b"MKKR" or struct.unpack_from("<I", b, 4)[0] != 1:
        return None
    n = struct.unpack_from("<I", b, 24)[0]
    slots = []
    for i in range(min(n, 16)):
        o = 32 + i * 128
        slots.append(dict(kind=b[o], log_n=b[o + 1], r=b[o + 2], p=b[o + 3], salt=bytes(b[o + 8:o + 24]),
                          nonce=bytes(b[o + 24:o + 48]), wrapped=bytes(b[o + 48:o + 80]),
                          tag=bytes(b[o + 80:o + 96])))
    return bytes(b[8:24]), slots


def _slot_aad(uuid, kind):
    return b"milkfs slot" + uuid + bytes([kind])


def parse_recovery(s):
    h = s.replace("-", "").replace(" ", "")
    if len(h) != 32:
        return None
    try:
        return bytes.fromhex(h)
    except ValueError:
        return None


def format_recovery(k):
    h = k.hex().upper()
    return "-".join(h[i:i + 4] for i in range(0, 32, 4))


def _kek(slot, secret):
    if slot["kind"] == SLOT_PASSPHRASE:
        return scrypt(secret, slot["salt"], slot["log_n"], slot["r"], slot["p"])
    return hmac256(slot["salt"], secret)


def wrap_slot(uuid, vk, kind, secret):
    sl = dict(kind=kind, log_n=KDF_LOG_N, r=KDF_R, p=KDF_P, salt=os.urandom(16), nonce=os.urandom(24))
    sl["wrapped"], sl["tag"] = xaead_seal(_kek(sl, secret), sl["nonce"], _slot_aad(uuid, kind), vk)
    return sl


def unlock_slots(uuid, slots, secret):
    """(volume key, slot index) for a passphrase or recovery key (str), or None."""
    rec = parse_recovery(secret)
    for i, sl in enumerate(slots):
        if sl["kind"] == SLOT_RECOVERY and rec:
            inp = rec
        elif sl["kind"] == SLOT_PASSPHRASE:
            inp = secret.encode()
        else:
            continue
        vk = xaead_open(_kek(sl, inp), sl["nonce"], _slot_aad(uuid, sl["kind"]), sl["wrapped"], sl["tag"])
        if vk is not None:
            return vk, i
    return None


# The secret for encrypted images: --passphrase-file FILE, --recovery KEY, $MILKFS_PASSPHRASE,
# or asked for on the terminal.
SECRET = {"value": None, "file": None, "recovery": None}
_KEYS = {}


def get_stream_secret(new=False):
    """An encrypted stream's passphrase: --stream-passphrase-file FILE,
    $MILKFS_STREAM_PASSPHRASE, or asked for (twice for a new one)."""
    if SECRET.get("stream_file"):
        return open(SECRET["stream_file"]).read().splitlines()[0]
    if os.environ.get("MILKFS_STREAM_PASSPHRASE"):
        return os.environ["MILKFS_STREAM_PASSPHRASE"]
    import getpass
    p = getpass.getpass("stream passphrase: ")
    if new and getpass.getpass("again: ") != p:
        sys.exit("mkmilkfs: the passphrases differ")
    return p


def get_secret(prompt="passphrase (or recovery key): "):
    if SECRET["recovery"]:
        return SECRET["recovery"]
    if SECRET["file"]:
        return open(SECRET["file"]).read().rstrip("\r\n")
    if os.environ.get("MILKFS_PASSPHRASE"):
        return os.environ["MILKFS_PASSPHRASE"]
    import getpass
    return getpass.getpass(prompt)


def volume_key(img, sb):
    """The unlocked key of an encrypted image (asks once per volume)."""
    if sb["uuid"] in _KEYS:
        return _KEYS[sb["uuid"]]
    p = sb["keyring"]
    ring = keyring_decode(img.raw(p[0])) if p[0] and xxh64(img.raw(p[0])) == p[2] else None
    if not ring:
        sys.exit(f"{img.path}: encrypted, but its keyring is unreadable")
    got = unlock_slots(ring[0], ring[1], get_secret())
    if not got:
        sys.exit(f"{img.path}: wrong passphrase or recovery key")
    k = VolumeKey(got[0])
    if k.sb_mac(sb["raw"]) != sb["sb_mac"]:  # over the bytes on disk
        sys.exit(f"{img.path}: the superblock's MAC doesn't match (altered?)")
    _KEYS[sb["uuid"]] = k
    return k


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
    b[4052:4076] = ptr_pack(sb.get("keyring", (0, 0, 0)))
    struct.pack_into("<Q", b, 4076, sb.get("sb_mac", 0))
    # No space map: this tool never writes one, so any commit from here drops the kernel's
    # (it would be stale); the next mount rebuilds it.
    struct.pack_into("<Q", b, BLOCK - 8, xxh64(bytes(b[:BLOCK - 8])))
    return bytes(b)


def sb_decode(b):
    if b[0:8] != MAGIC or struct.unpack_from("<Q", b, BLOCK - 8)[0] != xxh64(b[:BLOCK - 8]):
        return None
    ver, bs, total, gen = struct.unpack_from("<IIQQ", b, 8)
    if not MIN_VERSION <= ver <= MAX_VERSION or bs != BLOCK:
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
                spacemap=ptr_unpack(b, 4016), keyring=ptr_unpack(b, 4052),
                sb_mac=struct.unpack_from("<Q", b, 4076)[0], raw=bytes(b),
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
    def __init__(self, path, unlock=True):
        self.path = path
        self.f = open(path, "rb")
        self.size = os.fstat(self.f.fileno()).st_size
        self.key = None
        sb = self.superblock()
        self.converting = bool(sb and sb["flags"] & SB_CONVERTING)
        self.forms = {"plain": set(), "sealed": set()}  # blocks read, by form
        if unlock and sb and sb["flags"] & (SB_ENCRYPTED | SB_CONVERTING):
            self.key = volume_key(self, sb)

    def raw(self, blk):
        self.f.seek(blk * BLOCK)
        b = self.f.read(BLOCK)
        return b + bytes(BLOCK - len(b))

    def read(self, p):
        blk, birth, csum = p
        b = self.raw(blk)
        if self.converting and xxh64(b) == csum:
            self.forms["plain"].add(blk)
            return b  # not converted yet: plain
        if self.key:
            pt = self.key.open(blk, birth, csum, b)
            if pt is None:
                raise IOError(f"checksum (tag) mismatch in block {blk} (birth {birth})")
            self.forms["sealed"].add(blk)
            return pt
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


def cmd_mkfs(path, size, label, force, version=None, encrypt=False, recovery_out=None):
    if os.path.exists(path):
        img = Image(path, unlock=False)  # only "is there a milkfs?" (no key needed)
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
    ring = None
    if encrypt:
        secret = new_passphrase()
        vk, rec = os.urandom(32), os.urandom(16)
        key = VolumeKey(vk)
        slots = [wrap_slot(sb["uuid"], vk, SLOT_PASSPHRASE, secret.encode()),
                 wrap_slot(sb["uuid"], vk, SLOT_RECOVERY, rec)]
        ring = keyring_encode(sb["uuid"], slots)
        leaf, tag = key.seal(2, 1, leaf)
        sb.update(root=(2, 1, tag), keyring=(3, 1, xxh64(ring)), flags=SB_ENCRYPTED,
                  version=CRYPT_VERSION)
        sb["sb_mac"] = key.sb_mac(sb_encode(sb))
    mode = "r+b" if os.path.exists(path) else "w+b"
    with open(path, mode) as f:
        f.truncate(size)
        f.seek(0)
        f.write(bytes(BLOCK))          # slot 0: no superblock
        f.write(sb_encode(sb))         # slot 1: generation 1
        f.write(leaf)                  # block 2: root leaf
        if ring:
            f.write(ring)              # block 3: keyring
        f.flush()
        os.fsync(f.fileno())
    print(f"{path}: milkfs v{sb['version']}, {total} blocks of 4 KiB, label {label!r}"
          f"{', encrypted' if ring else ''}")
    if ring:
        msg = (f"Recovery key: {format_recovery(rec)}\n"
               "Write it down and keep it somewhere safe: it unlocks the volume if the passphrase\n"
               "is forgotten, and nothing else can.")
        if recovery_out:
            with open(recovery_out, "w") as f:
                os.fchmod(f.fileno(), 0o600)
                f.write(format_recovery(rec) + "\n")
            print(f"recovery key written to {recovery_out}")
        else:
            print(msg)


def new_passphrase():
    """A new passphrase: --new-passphrase-file, $MILKFS_NEW_PASSPHRASE, or asked twice."""
    if SECRET.get("new_file"):
        p = open(SECRET["new_file"]).read().rstrip("\r\n")
    elif os.environ.get("MILKFS_NEW_PASSPHRASE"):
        p = os.environ["MILKFS_NEW_PASSPHRASE"]
    else:
        import getpass
        p = getpass.getpass("new passphrase: ")
        if getpass.getpass("again: ") != p:
            sys.exit("the passphrases differ")
    if len(p) < 8:
        sys.exit("a passphrase needs 8 characters or more")
    return p


def cmd_change_passphrase(path):
    """Rewrap the volume key under a new passphrase (the current passphrase or the
    recovery key unlocks it); a new keyring block and one commit, no data rewritten."""
    img = Image(path)
    sb = img.superblock()
    if not sb["flags"] & SB_ENCRYPTED:
        sys.exit(f"{path}: not encrypted")
    uuid, slots = keyring_decode(img.raw(sb["keyring"][0]))
    got = unlock_slots(uuid, slots, get_secret("current passphrase (or recovery key): "))
    if not got:
        sys.exit(f"{path}: wrong passphrase or recovery key")
    vk, idx = got
    new = new_passphrase()
    slot = wrap_slot(uuid, vk, SLOT_PASSPHRASE, new.encode())
    if slots[idx]["kind"] == SLOT_PASSPHRASE:
        slots[idx] = slot
    else:
        i = next((i for i, x in enumerate(slots) if x["kind"] == SLOT_PASSPHRASE), None)
        if i is None:
            slots.append(slot)
        else:
            slots[i] = slot
    w = Writer(path, img, sb)
    sb = dict(sb, keyring=w.put_plain(keyring_encode(uuid, slots), sb["gen"] + 1))
    w.commit(sb)
    print(f"{path}: passphrase changed (volume key rewrapped, no data rewritten)")
    return 0


def cmd_selftest_crypto():
    """The published vectors, on this tool's crypto (as the kernel does at boot)."""
    H = bytes.fromhex
    pt = (b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for "
          b"the future, sunscreen would be it.")
    k = bytes(range(32))
    ok = [
        ("chacha20 block", _chacha_block(k, 1, H("000000090000004a00000000")).hex().startswith("10f1e7e4d13b5915500fdd1fa32071c4")),
        ("poly1305", poly1305(H("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"),
                              b"Cryptographic Forum Research Group").hex() == "a8061dc1305136c6c22b8baf0c0127a9"),
        ("hchacha20", hchacha20(k, H("000000090000004a0000000031415927")).hex() ==
         "82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc"),
    ]
    ct, tag = xaead_seal(H("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f"),
                         H("404142434445464748494a4b4c4d4e4f5051525354555657"), H("50515253c0c1c2c3c4c5c6c7"), pt)
    ok.append(("xchacha20-poly1305", tag.hex() == "c0875924c1c7987947deafd8780acf49" and ct[:4].hex() == "bd6d179d"))
    ok.append(("scrypt", scrypt(b"password", b"NaCl", 10, 8, 16, 64).hex().startswith("fdbabe1c9d3472007856e719")))
    for name, good in ok:
        print(("ok   " if good else "FAIL ") + name)
    return 0 if all(g for _, g in ok) else 1


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
    if sb.get("keyring", (0, 0, 0))[0]:
        used.add(sb["keyring"][0])
    return used


def bref_check(items):
    """Shared-block table of one tree vs the references its data items make.
    Returns (problem text or None, {block: extra} the table should hold)."""
    refs = {}
    for k, v in items.items():
        for _, p in data_ptrs(k, v):
            refs[p[0]] = refs.get(p[0], 0) + 1
    want = {b: n - 1 for b, n in refs.items() if n > 1}
    have, overlap = {}, False
    for (obj, kind, off), v in items.items():
        if obj == VOL_OBJ and kind == K_BREF:
            ln, extra = struct.unpack_from("<II", v)
            for b in range(off, off + ln):
                overlap |= b in have
                have[b] = extra
    wrong = sorted(b for b in set(want) | set(have) if want.get(b) != have.get(b))
    if not wrong and not overlap:
        return None, want
    under = sum(1 for b in wrong if have.get(b, 0) < want.get(b, 0))
    b = wrong[0] if wrong else None
    return (f"shared-block table: {len(wrong)} block(s) counted wrong ({under} too low)"
            + (", entries overlap" if overlap else "")
            + (f", e.g. block {b}: {refs.get(b, 0)} references, table says {1 + have.get(b, 0)}" if b is not None else "")), want


def bref_rebuild(items, want):
    """Replace the tree's shared-block table with `want` ({block: extra})."""
    for k in [k for k in items if k[0] == VOL_OBJ and k[1] == K_BREF]:
        del items[k]
    run = None
    for b in sorted(want):
        x = want[b]
        if run and run[0] + run[1] == b and run[2] == x and run[1] < 128:
            run[1] += 1
            continue
        if run:
            items[(VOL_OBJ, K_BREF, run[0])] = struct.pack("<II", run[1], run[2])
        run = [b, 1, x]
    if run:
        items[(VOL_OBJ, K_BREF, run[0])] = struct.pack("<II", run[1], run[2])


def read_spacemap(img, sb):
    """(index ptr + chunk ptrs, set of blocks marked used) of the superblock's space map,
    None if it has none. Raises IOError/ValueError if it is damaged or stale."""
    p = sb.get("spacemap", (0, 0, 0))
    if p[0] == 0:
        return None
    b = img.read(p)
    if b[0:4] != b"MKSM":
        raise ValueError("bad space map magic")
    gen, total, used, n = struct.unpack_from("<QQQI", b, 8)
    if gen != sb["gen"]:
        raise ValueError(f"space map is of generation {gen}, not {sb['gen']}")
    if total != sb["total"] or n != (total + BLOCK * 8 - 1) // (BLOCK * 8):
        raise ValueError("space map doesn't match the volume size")
    chunks = [ptr_unpack(b, 40 + 24 * i) for i in range(n)]
    marked = set()
    for i, c in enumerate(chunks):
        data = img.read(c)
        for j in range(BLOCK // 8):
            w = struct.unpack_from("<Q", data, j * 8)[0]
            while w:
                bit = (w & -w).bit_length() - 1
                marked.add(i * BLOCK * 8 + j * 64 + bit)
                w &= w - 1
    if len(marked) != used:
        raise ValueError(f"space map count {used} != {len(marked)} bits set")
    return [p] + chunks, marked


def spacemap_problems(img, sb, used):
    """Compare the space map with the reachable blocks `used` (without the map's own)."""
    try:
        m = read_spacemap(img, sb)
    except (IOError, ValueError) as e:
        return [f"space map unreadable ({e}): the kernel rebuilds it at mount, --repair drops it"], set(), []
    if m is None:
        return [], set(), []
    own, marked = m
    want = used | {p[0] for p in own}
    out = []
    lost = want - marked
    extra = marked - want
    if lost:
        out.append(f"space map marks {len(lost)} block(s) in use as free (e.g. {sorted(lost)[:5]})")
    notes = []
    if extra:
        msg = f"space map marks {len(extra)} unreachable block(s) as used (e.g. {sorted(extra)[:5]})"
        if sb["flags"] & 1:
            # Branched history: blocks the live tree dropped wait for the next sweep.
            notes.append(msg + ": waiting for the next sweep (branched history)")
        else:
            out.append(msg)
    return out, {p[0] for p in own}, notes


class Writer:
    """Writes new blocks into blocks the current superblock doesn't reach."""

    def __init__(self, path, img, sb):
        if sb["flags"] & SB_CONVERTING:
            sys.exit(f"{path}: being converted (encrypted or decrypted) in place: "
                     "let milkOS finish that first")
        self.f = open(path, "r+b")
        self.key = img.key
        used = all_used(img, sb)
        self.free = (b for b in range(2, sb["total"]) if b not in used)

    def put(self, data, birth):
        blk = next(self.free)
        if self.key:
            data, csum = self.key.seal(blk, birth, data)
        else:
            csum = xxh64(data)
        self.f.seek(blk * BLOCK)
        self.f.write(data)
        return (blk, birth, csum)

    def put_plain(self, data, birth):
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

    def commit(self, sb, new_root=None, add_snaps=()):
        """New generation: the superblock write (after an fsync) is the commit point.
        add_snaps: snapshot records to add (their gen is filled in)."""
        sb2 = dict(sb, snaps=[dict(x) for x in sb["snaps"]], gen=sb["gen"] + 1, spacemap=(0, 0, 0))
        if new_root is not None and new_root != sb["root"]:
            sb2["root"] = new_root
            if sb2["snaps"]:
                # The live tree was rewritten outside the dead-list bookkeeping: switch
                # the volume to exact sweeps (like a promote does).
                sb2["flags"] = sb2.get("flags", 0) | 1
                sb2["dead"] = (0, 0, 0)
                for x in sb2["snaps"]:
                    x["dead"] = (0, 0, 0)
        for x in add_snaps:
            sb2["snaps"].append(dict(x, gen=sb2["gen"], root=sb2["root"]))
        if self.key:
            sb2["version"] = max(sb2.get("version", 0), CRYPT_VERSION)
            sb2["sb_mac"] = 0
            sb2["sb_mac"] = self.key.sb_mac(sb_encode(sb2))
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
    for (obj, kind, off) in live:
        if obj == VOL_OBJ and kind == K_ORPHAN and (inodes.get(off) or {}).get("nlink", 1) != 0:
            problems.append(f"orphan list names inode {off}, which is gone or has links")
            plan.setdefault("stale_orphans", []).append(off)
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
        items.pop((VOL_OBJ, K_ORPHAN, ino), None)
        log.append(f"deleted orphan inode {ino}")
    for ino in plan.get("stale_orphans", []):
        items.pop((VOL_OBJ, K_ORPHAN, ino), None)
        log.append(f"dropped orphan-list entry for inode {ino}")
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
    bad_bref, _ = bref_check(live)
    if bad_bref:
        problems.append(bad_bref)
    map_bad, _, _ = spacemap_problems(img, sb, all_used(img, sb))
    problems += map_bad
    log = []
    if problems or bad_slot:
        w = Writer(path, img, sb)
        sb = dict(sb)
        if any(plan.values()) or bad_bref:
            apply_repairs(live, sb, plan, log)
            # Deleted orphans may have shared blocks: the table follows the references.
            bad_after, want = bref_check(live)
            if bad_after:
                bref_rebuild(live, want)
                log.append("shared-block table rebuilt from the references")
            if want:
                sb["version"] = max(sb.get("version", 1), 2)
            new_root = w.tree(live, sb["gen"] + 1)
        else:
            new_root = None
        sb2 = w.commit(sb, new_root)
        if map_bad:
            log.append("space map dropped (the next mount rebuilds it from the trees)")
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


def cmd_mkdir(path, dest, mode=0o755):
    img = Image(path)
    sb = dict(img.superblock())
    sb["version"] = max(sb.get("version", 0), 1)
    live = dict(img.items(sb["root"]))
    w = Writer(path, img, sb)
    now = time.time()
    d, made = ROOT_INO, False
    for part in [x for x in dest.split("/") if x]:
        hit = [ino for (o, k, _), v in live.items() if o == d and k == K_DIRENT
               for nm, ino, _ in dirents_decode(v) if nm == part]
        if hit:
            d = hit[0]
            continue
        ino = sb["next_ino"]
        sb["next_ino"] += 1
        live[(ino, K_INODE, 0)] = inode_encode(S_IFDIR | mode, 2, 0, d, now)
        bucket_edit(live, d, part, lambda l, n=part, i=ino: l + [(n, i, 4)])
        set_inode_field(live, d, 4, "<I", inode_decode(live[(d, K_INODE, 0)])["nlink"] + 1)
        d, made = ino, True
    if made:
        w.commit(sb, w.tree(live, sb["gen"] + 1))
    print(f"{path}: {dest}{'' if made else ' (exists)'}")
    return 0


FRUITS = ["apple", "banana", "cherry", "grape", "lemon", "mango", "peach"]


def cmd_populate(path, dest, n):
    """N files under DEST/dNNN/fNNNNN, in one commit (mount-time and big-tree tests)."""
    img = Image(path)
    sb = dict(img.superblock())
    sb["version"] = max(sb.get("version", 0), 1)
    live = dict(img.items(sb["root"]))
    w = Writer(path, img, sb)
    gen = sb["gen"] + 1
    now = time.time()

    def mkdir(parent, name):
        ino = sb["next_ino"]
        sb["next_ino"] += 1
        live[(ino, K_INODE, 0)] = inode_encode(S_IFDIR | 0o755, 2, 0, parent, now)
        bucket_edit(live, parent, name, lambda l: l + [(name, ino, 4)])
        set_inode_field(live, parent, 4, "<I", inode_decode(live[(parent, K_INODE, 0)])["nlink"] + 1)
        return ino

    d = ROOT_INO
    for part in [x for x in dest.split("/") if x]:
        hit = [ino for (o, k, _), v in live.items() if o == d and k == K_DIRENT
               for nm, ino, _ in dirents_decode(v) if nm == part]
        d = hit[0] if hit else mkdir(d, part)
    sub = None
    for i in range(n):
        if i % 100 == 0:
            sub = mkdir(d, f"d{i // 100:03}")
        ino = sb["next_ino"]
        sb["next_ino"] += 1
        name = f"f{i:05}" + [".md", ".txt", ""][i % 3]
        line = f"file {i} of {n} uniq{i:05} {FRUITS[i % 7]}\n".encode()
        if i % 50 == 49:
            data = (line + f"big file {i}\n".encode() * 700)[:8192]
            blocks = [data[j:j + BLOCK].ljust(BLOCK, b"\0") for j in range(0, len(data), BLOCK)]
            ptrs = [w.put(b, gen) for b in blocks]
            for j, ptr in enumerate(ptrs):
                live[(ino, K_EXT, j)] = struct.pack("<QQIQ", ptr[0], gen, 1, ptr[2])
        else:
            data = (line + f"more text {i}\n".encode() * (i % 40))[:1024]
            live[(ino, K_INLINE, 0)] = data
        live[(ino, K_INODE, 0)] = inode_encode(S_IFREG | 0o644, 1, len(data), sub, now)
        bucket_edit(live, sub, name, lambda l: l + [(name, ino, 8)])
    w.commit(sb, w.tree(live, gen))
    print(f"{path}: {n} files under {dest}")
    return 0


# ---- send streams ---------------------------------------------------------------------
R_HEADER, R_ITEM, R_DELETE, R_DROP, R_DATA, R_EXTENT, R_END = 1, 2, 3, 4, 5, 6, 7
R_NAMES = {1: "header", 2: "item", 3: "delete", 4: "drop", 5: "data", 6: "extent", 7: "end"}


class StreamError(Exception):
    pass


def chain_step(chain, csum):
    return xxh64(struct.pack("<QQ", chain, csum))


SEALED_MAGIC, ENVELOPE_LEN, SEAL_LAST = b"MILKSTRX", 128, 1
_STREAM_KEYS = {}


class StreamKey:
    """An encrypted stream's key and record counter (layout: kernel crypt.rs StreamKey)."""

    def __init__(self, key, prefix):
        self.key, self.prefix, self.next = key, prefix, 0

    @staticmethod
    def wrap_aad(env):
        return b"milkfs stream key" + env[:48]

    @classmethod
    def create(cls, passphrase):
        env = bytearray(ENVELOPE_LEN)
        env[0:8] = SEALED_MAGIC
        env[8:16] = struct.pack("<I4B", 1, 16, 8, 4, 1)
        env[16:72] = os.urandom(56)  # salt, record nonce prefix, wrap nonce
        sk = os.urandom(32)
        kek = scrypt(passphrase.encode(), bytes(env[16:32]), 16, 8, 4)
        ct, tag = xaead_seal(kek, bytes(env[48:72]), cls.wrap_aad(bytes(env)), sk)
        env[72:104], env[104:120] = ct, tag
        return cls(sk, bytes(env[32:48])), bytes(env)

    @classmethod
    def open(cls, env, passphrase):
        ver, log_n, r, p, kind = struct.unpack_from("<I4B", env, 8)
        if env[:8] != SEALED_MAGIC or ver != 1 or kind != 1 or env[120:] != bytes(8) \
                or not 1 <= log_n <= 20 or 128 * r * (1 << log_n) > 128 << 20 or not 1 <= p <= 16:
            raise StreamError("an encrypted stream this tool can't read (version or parameters)")
        kek = scrypt(passphrase.encode(), env[16:32], log_n, r, p)
        sk = xaead_open(kek, env[48:72], cls.wrap_aad(env), env[72:104], env[104:120])
        if sk is None:
            raise StreamError("wrong passphrase for the stream (or its envelope was altered)")
        return cls(sk, env[32:48])

    def _na(self, ln, flags):
        return (self.prefix + struct.pack("<Q", self.next),
                b"MKSR" + struct.pack("<QII", self.next, ln, flags))

    def seal(self, rec, last):
        flags = SEAL_LAST if last else 0
        n, a = self._na(len(rec), flags)
        ct, tag = xaead_seal(self.key, n, a, rec)
        self.next += 1
        return struct.pack("<II", len(rec), flags) + ct + tag

    def open_record(self, hdr, body):
        ln, flags = struct.unpack("<II", hdr)
        if len(body) != ln + 16 or flags & ~SEAL_LAST:
            raise StreamError(f"bad sealed record {self.next}")
        n, a = self._na(ln, flags)
        pt = xaead_open(self.key, n, a, body[:ln], body[ln:])
        if pt is None:
            raise StreamError(f"sealed record {self.next} doesn't authenticate (altered, reordered or replaced)")
        self.next += 1
        return pt, flags


def stream_is_sealed(path):
    with open(path, "rb") as f:
        return f.read(8) == SEALED_MAGIC


def _raw_records(path):
    """Yield the raw bytes (header | payload | xxh64) of each record, opening an encrypted
    stream with its passphrase (asked once per file)."""
    f = open(path, "rb")
    m = f.read(8)
    if m != SEALED_MAGIC:
        f.seek(0)
        while True:
            h = f.read(8)
            if len(h) < 8:
                raise StreamError("truncated (no END record)")
            ln = struct.unpack_from("<I", h, 4)[0]
            if h[1:4] != b"\0\0\0" or ln > 64 << 10:
                raise StreamError(f"bad record header at byte {f.tell() - 8}")
            rest = f.read(ln + 8)
            if len(rest) < ln + 8:
                raise StreamError("truncated record")
            yield h + rest
            if h[0] == R_END:
                return
    env = m + f.read(ENVELOPE_LEN - 8)
    if len(env) < ENVELOPE_LEN:
        raise StreamError("truncated envelope")
    if path not in _STREAM_KEYS:
        _STREAM_KEYS[path] = get_stream_secret()
    k = StreamKey.open(env, _STREAM_KEYS[path])
    last = False
    while True:
        h = f.read(8)
        if not h and last:
            return
        if len(h) < 8:
            raise StreamError("truncated (no END record)")
        if last:
            raise StreamError("data after the last record")
        ln = struct.unpack_from("<I", h)[0]
        if ln > (64 << 10) + 16:
            raise StreamError("bad sealed record length")
        body = f.read(ln + 16)
        if len(body) < ln + 16:
            raise StreamError("truncated record")
        rec, flags = k.open_record(h, body)
        last = bool(flags & SEAL_LAST)
        if len(rec) < 16 or struct.unpack_from("<I", rec, 4)[0] + 16 != len(rec) or (rec[0] == R_END) != last:
            raise StreamError(f"bad record inside sealed record {k.next - 1}")
        yield rec


def read_stream(path):
    """Yield (type, payload) of every record, checking each checksum and, at the end, the
    counts and the chained hash. The header is decoded into a dict (type R_HEADER). An
    encrypted stream is opened with its passphrase (get_stream_secret)."""
    records = data = chain = 0
    first = True
    for rec in _raw_records(path):
        h, t, ln = rec[:8], rec[0], struct.unpack_from("<I", rec, 4)[0]
        p, c = rec[8:8 + ln], struct.unpack_from("<Q", rec, 8 + ln)[0]
        if xxh64(h + p) != c:
            raise StreamError(f"checksum mismatch in record {records} ({R_NAMES.get(t, t)})")
        if first and t != R_HEADER:
            raise StreamError("no header")
        first = False
        if t == R_END:
            n, d, ch = struct.unpack("<QQQ", p)
            if (n, d, ch) != (records, data, chain):
                raise StreamError("END doesn't match the records (missing, reordered or replaced)")
            yield t, (n, d)
            return
        chain = chain_step(chain, c)
        records += 1
        if t == R_DATA:
            data += 1
        if t == R_HEADER:
            if p[0:8] != b"MILKSTRM":
                raise StreamError("not a milkfs stream")
            ver, flags = struct.unpack_from("<II", p, 8)
            if ver != 1:
                raise StreamError(f"stream version {ver} (this tool reads 1)")
            hd = dict(incremental=bool(flags & 1), uuid=p[16:32].hex(), label=cstr(p[32:64]),
                      snap=cstr(p[64:112]), snap_created=struct.unpack_from("<Q", p, 112)[0],
                      snap_gen=struct.unpack_from("<Q", p, 120)[0], base=cstr(p[128:176]),
                      base_created=struct.unpack_from("<Q", p, 176)[0],
                      base_gen=struct.unpack_from("<Q", p, 184)[0],
                      next_ino=struct.unpack_from("<Q", p, 192)[0])
            yield t, hd
        else:
            yield t, p


def cmd_seal_stream(src, dst):
    """Encrypt a plaintext stream (after checking it all) under a new passphrase."""
    if stream_is_sealed(src):
        sys.exit(f"{src}: already encrypted")
    try:
        recs = list(_raw_records(src))
        for _ in read_stream(src):
            pass
    except StreamError as e:
        print(f"{src}: BAD stream: {e}")
        return 1
    p = get_stream_secret(new=True)
    if len(p) < 8:
        sys.exit("mkmilkfs: a stream passphrase needs 8 characters or more")
    k, env = StreamKey.create(p)
    with open(dst, "wb") as f:
        f.write(env)
        for i, r in enumerate(recs):
            f.write(k.seal(r, i == len(recs) - 1))
    print(f"{dst}: {len(recs)} records sealed")
    return 0


def key_unpack(p):
    return struct.unpack_from("<QBQ", p)


def cmd_verify_stream(path):
    counts, hd = {}, None
    try:
        for t, p in read_stream(path):
            counts[t] = counts.get(t, 0) + 1
            if t == R_HEADER:
                hd = p
    except StreamError as e:
        print(f"{path}: BAD stream: {e}")
        return 1
    kind = f"incremental from {hd['base']!r} (created {hd['base_created']})" if hd["incremental"] else "full"
    enc = ", encrypted (XChaCha20-Poly1305 records, scrypt-wrapped key)" if stream_is_sealed(path) else ""
    print(f"{path}: milkfs stream v1{enc}, snapshot {hd['snap']!r} of {hd['label']!r} "
          f"(created {hd['snap_created']}), {kind}")
    print("  " + ", ".join(f"{counts[t]} {R_NAMES[t]}" for t in sorted(counts) if t not in (R_HEADER, R_END)))
    print(f"  {os.path.getsize(path)} bytes, every checksum and the END record check out")
    return 0


class DataIndex:
    """Data item keys per inode, for file-range drops on an item dict."""

    def __init__(self, items):
        self.items, self.by = items, {}
        for k in items:
            if k[1] in (K_EXT, K_EXTENT):
                self.by.setdefault(k[0], set()).add(k)

    def put(self, k, v):
        self.items[k] = v
        self.by.setdefault(k[0], set()).add(k)

    def drop(self, ino, s, e):
        for k in list(self.by.get(ino, ())):
            ptrs = data_ptrs(k, self.items[k])
            start, end = ptrs[0][0], ptrs[-1][0] + 1
            if end <= s or start >= e:
                continue
            del self.items[k]
            self.by[ino].discard(k)
            for part in ([p for p in ptrs if p[0] < s], [p for p in ptrs if p[0] >= e]):
                if part:
                    self.put((ino, K_EXT, part[0][0]), ext_pack([p for _, p in part]))


def ext_pack(ptrs):
    """K_EXT value for consecutive blocks with one birth."""
    return struct.pack("<QQI", ptrs[0][0], ptrs[0][1], len(ptrs)) + b"".join(struct.pack("<Q", p[2]) for p in ptrs)


def cmd_receive(path, stream, force):
    img = Image(path)
    sb = img.superblock()
    try:
        for _ in read_stream(stream):  # check it all before writing anything
            pass
    except StreamError as e:
        print(f"{stream}: BAD stream: {e} (nothing written to {path})")
        return 1
    try:
        recs = read_stream(stream)
        _, hd = next(recs)
        if any(s["name"] == hd["snap"] for s in sb["snaps"]):
            print(f"{path}: already has a snapshot {hd['snap']!r}")
            return 2
        if hd["incremental"]:
            base = next((s for s in sb["snaps"] if s["name"] == hd["base"]
                         and s["created"] == hd["base_created"] and not s["clone"]), None)
            if base is None:
                print(f"{path}: the stream's base snapshot {hd['base']!r} (created {hd['base_created']}) isn't here")
                return 2
            if base["root"] != sb["root"] and not force:
                print(f"{path}: changed since snapshot {hd['base']!r} (--force rolls it back to it)")
                return 2
            live = dict(img.items(base["root"]))
        else:
            live = dict(img.items(sb["root"]))
            if any(k[0] == ROOT_INO and k[1] == K_DIRENT for k in live):
                print(f"{path}: a full stream needs an empty volume")
                return 2
        sb = dict(sb)
        sb["version"] = max(sb.get("version", 0), 1)
        w = Writer(path, img, sb)
        gen = sb["gen"] + 1
        idx = DataIndex(live)
        blocks, max_obj, n = {}, 0, 0
        for t, p in recs:
            n += 1
            if t == R_ITEM:
                k = key_unpack(p)
                live[k] = p[17:]
                max_obj = max(max_obj, k[0])
            elif t == R_DELETE:
                live.pop(key_unpack(p), None)
            elif t == R_DROP:
                idx.drop(*struct.unpack("<QQQ", p))
            elif t == R_DATA:
                src = struct.unpack_from("<Q", p)[0]
                # (our pointer, xxh64 of the plaintext: what the stream's extents name)
                blocks[src] = (w.put(p[8:], gen), xxh64(p[8:]))
            elif t == R_EXTENT:
                ino, start, old_end, src, cnt = struct.unpack_from("<QQQQI", p)
                csums = struct.unpack_from(f"<{cnt}Q", p, 36)
                idx.drop(ino, start, max(start + cnt, old_end))
                ptrs = []
                for i in range(cnt):
                    b = blocks.get(src + i)
                    if b is None or b[1] != csums[i]:
                        raise StreamError(f"extent of inode {ino} refers to data the stream didn't carry")
                    ptrs.append(b[0])
                i = 0
                while i < cnt:
                    j = i + 1
                    while j < cnt and ptrs[j][0] == ptrs[i][0] + (j - i):
                        j += 1
                    idx.put((ino, K_EXT, start + i), ext_pack(ptrs[i:j]))
                    i = j
                max_obj = max(max_obj, ino)
    except StreamError as e:
        print(f"{stream}: BAD stream: {e} (nothing written to {path})")
        return 1
    _, want = bref_check(live)
    bref_rebuild(live, want)
    if want:
        sb["version"] = max(sb["version"], 2)
    sb["next_ino"] = max(sb["next_ino"], hd["next_ino"], max_obj + 1)
    root = w.tree(live, gen)
    snap = dict(name=hd["snap"], created=hd["snap_created"], dead=(0, 0, 0),
                next_ino=sb["next_ino"], clone=False, id=0)
    w.commit(sb, root, add_snaps=[snap])
    print(f"{path}: received {hd['snap']!r} ({'incremental' if hd['incremental'] else 'full'}, {n} records, "
          f"{len(blocks)} data blocks{', ' + str(len(want)) + ' shared' if want else ''})")
    return 0


def cmd_tree(path, snapshot):
    img = Image(path)
    t = tree_for(img, img.superblock(), snapshot)

    def walk(ino, prefix):
        for name, cino, _ in sorted(t.listdir(ino)):
            i = t.inode(cino)
            p = f"{prefix}/{name}"
            kind = i["mode"] & S_IFMT
            if kind == S_IFDIR:
                print(f"d {i['mode'] & 0o7777:o} {p}")
                walk(cino, p)
            elif kind == S_IFLNK:
                tgt = t.items.get((cino, K_SYMLINK, 0), b"").decode("utf-8", "replace")
                print(f"l {i['mode'] & 0o7777:o} {p} -> {tgt}")
            else:
                data = t.read(cino)
                print(f"- {i['mode'] & 0o7777:o} {i['size']:9} {i['mtime']} {xxh64(data):016x} {p}")
    walk(ROOT_INO, "")


def cmd_flip(path, fpath, snapshot):
    img = Image(path)
    t = tree_for(img, img.superblock(), snapshot)
    ino = t.resolve(fpath)
    ptrs = sorted(p for k, v in t.items.items() if k[0] == ino for p in data_ptrs(k, v))
    if not ptrs:
        sys.exit(f"{fpath}: no data blocks (inline or empty)")
    blk = ptrs[0][1][0]
    with open(path, "r+b") as f:
        f.seek(blk * BLOCK + 100)
        b = f.read(1)
        f.seek(blk * BLOCK + 100)
        f.write(bytes([b[0] ^ 1]))
        f.flush()
        os.fsync(f.fileno())
    print(f"{path}: flipped a bit in block {blk} (first data block of {fpath}"
          f"{' in snapshot ' + snapshot if snapshot else ''})")
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
    elif kind == "bref":
        f.close()
        ents = sorted(k for k in live if k[0] == VOL_OBJ and k[1] == K_BREF)
        if not ents:
            sys.exit("no shared blocks to damage")
        ln, extra = struct.unpack_from("<II", live[ents[0]])
        del live[ents[0]]  # a shared run now counts as referenced once: too low
        w = Writer(path, img, sb)
        w.commit(sb, w.tree(live, sb["gen"] + 1))
        print(f"{path}: corrupted (bref)")
        return 0
    elif kind == "smap":
        m = read_spacemap(img, sb)
        if m is None:
            sys.exit("no space map to damage")
        f.seek(m[0][-1][0] * BLOCK)  # a bitmap chunk
        f.write(b"\x5a" * BLOCK)
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
    if sb.get("keyring", (0, 0, 0))[0]:
        used.add(sb["keyring"][0])  # encrypted: the keyring block

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

    notes = []
    live = walk(sb["root"], "live")
    trees = [("live", live)]
    for s in sb["snaps"]:
        what = f"{'clone' if s['clone'] else 'snapshot'} {s['name']}"
        trees.append((what, walk(s["root"], what)))
    shared = 0
    for what, items in trees:
        bad, want = bref_check(items)
        if bad and what == "live":
            problems.append(f"{what}: {bad}")
        elif bad:
            # Snapshots are read-only: one taken while the live table was wrong keeps it.
            # Harmless until it is rolled back to (or cloned): then check --repair.
            notes.append(f"{what}: {bad} (taken before a repair? after a rollback to it, run check --repair)")
        if what == "live":
            shared = len(want)
        if want and sb["version"] < 2:
            problems.append(f"{what}: shares blocks but the volume is v{sb['version']} (older kernels would free them)")
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
    for (obj, kind, off) in live:
        if obj == VOL_OBJ and kind == K_ORPHAN and (inodes.get(off) or {}).get("nlink", 1) != 0:
            problems.append(f"orphan list names inode {off}, which is gone or has links")
    if ROOT_INO not in inodes:
        problems.append("no root directory")
    other = (sb["gen"] + 1) % 2
    o = sb_decode(img.raw(other))
    if sb["gen"] > 1 and (o is None or o["gen"] >= sb["gen"]):
        # Not damage: what a commit interrupted mid-superblock-write leaves; the next
        # commit overwrites that slot. --repair rewrites it now.
        notes.append(f"superblock slot {other} is torn (an interrupted commit; the next commit rewrites it)")

    map_bad, map_own, map_notes = spacemap_problems(img, sb, used)
    problems += map_bad
    notes += map_notes
    used |= map_own
    if not quiet or problems:
        print(f"{path}: milkfs v{sb['version']} label {sb['label']!r} generation {sb['gen']}, "
              f"{len(inodes)} inodes, {len(used)}/{sb['total']} blocks used, "
              f"{len(sb['snaps'])} snapshot(s), {orphans} orphan(s)")
        if shared:
            print(f"  {shared} data block(s) shared by reflink copies")
        print("  space map:", "current" if map_own and not map_bad else
              "none (the next mount rebuilds it)" if not sb["spacemap"][0] else "BAD")
        if sb["flags"] & 1:
            print("  history branched (a clone was promoted): no dead lists, sweeps reclaim space")
        if sb["flags"] & SB_CONVERTING:
            print(f"  encryption: being {'decrypted' if sb['flags'] & SB_TO_PLAIN else 'encrypted'} in place: "
                  f"{len(img.forms['sealed'])} block(s) read sealed, {len(img.forms['plain'])} plain")
        elif sb["flags"] & SB_ENCRYPTED:
            print("  encryption: on")
        else:
            print("  encryption: off")
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
    if not argv or (len(argv) < 2 and argv[0] != "selftest-crypto"):
        sys.exit(__doc__)
    cmd, args = argv[0], argv[1:]
    opts, pos = {}, []
    i = 0
    while i < len(args):
        a = args[i]
        if a in ("--size", "--label", "--snapshot", "--passphrase-file", "--recovery",
                 "--new-passphrase-file", "--recovery-out", "--stream-passphrase-file"):
            opts[a] = args[i + 1]
            i += 2
        elif a.startswith("--"):
            opts[a] = True
            i += 1
        else:
            pos.append(a)
            i += 1
    SECRET.update(file=opts.get("--passphrase-file"), recovery=opts.get("--recovery"),
                  new_file=opts.get("--new-passphrase-file"),
                  stream_file=opts.get("--stream-passphrase-file"))
    if cmd == "mkfs":
        if "--encrypt" in opts and "--passphrase-file" in opts and "--new-passphrase-file" not in opts:
            SECRET["new_file"] = opts["--passphrase-file"]
        cmd_mkfs(pos[0], parse_size(opts["--size"]) if "--size" in opts else None,
                 opts.get("--label", "Milk"), "--force" in opts, 0 if "--v0" in opts else None,
                 "--encrypt" in opts, opts.get("--recovery-out"))
    elif cmd == "change-passphrase":
        return cmd_change_passphrase(pos[0])
    elif cmd == "selftest-crypto":
        return cmd_selftest_crypto()
    elif cmd == "check":
        if "--repair" in opts:
            return cmd_check_repair(pos[0])
        return cmd_check(pos[0], "--quiet" in opts)
    elif cmd == "corrupt" and pos[1] == "flip":
        return cmd_flip(pos[0], pos[2], opts.get("--snapshot"))
    elif cmd == "corrupt":
        return cmd_corrupt(pos[0], pos[1])
    elif cmd == "verify-stream":
        return cmd_verify_stream(pos[0])
    elif cmd == "seal-stream":
        return cmd_seal_stream(pos[0], pos[1])
    elif cmd == "receive":
        return cmd_receive(pos[0], pos[1], "--force" in opts)
    elif cmd == "tree":
        cmd_tree(pos[0], opts.get("--snapshot"))
    elif cmd == "mkdir":
        return cmd_mkdir(pos[0], pos[1], int(pos[2], 8) if len(pos) > 2 else 0o755)
    elif cmd == "populate":
        return cmd_populate(pos[0], pos[1], int(pos[2]))
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
