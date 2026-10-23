#!/usr/bin/env python3
"""Build the TLS root store: the Mozilla CA bundle (the NSS built-in roots that Mozilla trusts
for websites), as DER, for the tls crate to compile in.

Usage:
    python3 -I scripts/gen_roots.py [BUNDLE]        regenerate from BUNDLE (default below)
    python3 -I scripts/gen_roots.py --check         regenerate in memory and compare with the
                                                    committed files (exit 1 if they differ)
    python3 -I scripts/gen_roots.py --new BUNDLE    take a newer bundle: print the values to
                                                    record in SOURCE below, then regenerate

Source: curl's mk-ca-bundle extraction of Mozilla's certdata.txt (only roots with the
websites trust bit, CKA_TRUST_SERVER_AUTH = CKT_NSS_TRUSTED_DELEGATOR), as shipped by
Homebrew's ca-certificates package at /opt/homebrew/share/ca-certificates/cacert.pem. This
repository has no network access in its tests, so the bundle is read from a local copy, and
the script refuses a file whose SHA-256 differs from the one recorded below (pass --new to
move to a newer bundle, and update SOURCE in the same commit).

Writes (all in assets/ca/):
    mozilla-roots.der   the roots' DER certificates, concatenated in bundle order; the tls
                        crate splits them at load time (each is one SEQUENCE)
    mozilla-roots.txt   the manifest: source, date, bundle SHA-256, and per root its
                        SHA-256 fingerprint, key and label

The certificate data is from Mozilla's NSS and is under the Mozilla Public License 2.0
(assets/ca/MPL-2.0.txt, assets/ca/NOTICE.txt).
"""
import base64
import hashlib
import pathlib
import re
import sys

SOURCE = {
    "path": "/opt/homebrew/share/ca-certificates/cacert.pem",
    "what": "curl's cacert.pem (mk-ca-bundle.pl 1.33), via Homebrew ca-certificates 2026-09-25",
    "mozilla_date": "Fri Sep 25 03:12:01 2026 GMT",
    "certdata_sha256": "beb7e6dfe6499926e52c075c27bcfbe4c957f8609c575b3860273ae2806f63eb",
    "bundle_sha256": "a41b5d356aea97a529fe27e0f7316d2f9d946d75927476cf9cf1b90637d00505",
    "count": 121,
}

REPO = pathlib.Path(__file__).resolve().parent.parent
OUT_DER = REPO / "assets/ca/mozilla-roots.der"
OUT_TXT = REPO / "assets/ca/mozilla-roots.txt"


def der_len(b, i):
    """(length, offset after the length octets) of the DER TLV whose length starts at b[i]."""
    first = b[i]
    if first < 0x80:
        return first, i + 1
    n = first & 0x7F
    if n == 0 or n > 4:
        raise ValueError("bad DER length")
    return int.from_bytes(b[i + 1:i + 1 + n], "big"), i + 1 + n


def describe_key(der):
    """A short description of the certificate's public key (for the manifest only)."""
    if b"\x2a\x86\x48\xce\x3d\x02\x01" in der:  # id-ecPublicKey
        if b"\x2a\x86\x48\xce\x3d\x03\x01\x07" in der:
            return "ecdsa-p256"
        if b"\x2b\x81\x04\x00\x22" in der:
            return "ecdsa-p384"
        if b"\x2b\x81\x04\x00\x23" in der:
            return "ecdsa-p521"
        return "ecdsa-?"
    if b"\x2b\x65\x70" in der:
        return "ed25519"
    m = re.search(rb"\x06\x09\x2a\x86\x48\x86\xf7\x0d\x01\x01\x01\x05\x00\x03(\x82..|\x81.)\x00\x30(\x82..|\x81.)\x02(\x82..|\x81.)",
                  der, re.S)
    if m:
        ln_field = m.group(3)
        n = int.from_bytes(ln_field[1:], "big")
        start = m.end()
        lead = der[start]
        bits = (n - (1 if lead == 0 else 0)) * 8
        return f"rsa-{bits}"
    return "?"


def parse_bundle(text):
    """[(label, der)] in file order. A label is the '====' underlined line above each PEM."""
    out = []
    lines = text.splitlines()
    i = 0
    label = "?"
    while i < len(lines):
        line = lines[i]
        if i + 1 < len(lines) and re.fullmatch(r"=+", lines[i + 1].strip() or "x") and line.strip():
            label = line.strip()
        if line.strip() == "-----BEGIN CERTIFICATE-----":
            j = i + 1
            body = []
            while lines[j].strip() != "-----END CERTIFICATE-----":
                body.append(lines[j].strip())
                j += 1
            der = base64.b64decode("".join(body), validate=True)
            n, off = der_len(der, 1)
            if der[0] != 0x30 or off + n != len(der):
                raise SystemExit(f"gen_roots: {label}: not a single DER SEQUENCE")
            out.append((label, der))
            i = j
        i += 1
    return out


def build(bundle_path, accept_new=False):
    raw = pathlib.Path(bundle_path).read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    text = raw.decode("utf-8")
    m = re.search(r"Certificate data from Mozilla as of: (.*)", text)
    when = m.group(1).strip() if m else "?"
    m = re.search(r"## SHA256: ([0-9a-f]{64})", text)
    certdata = m.group(1) if m else "?"
    roots = parse_bundle(text)
    if accept_new:
        print("record these in SOURCE:")
        print(f'    "mozilla_date": "{when}",')
        print(f'    "certdata_sha256": "{certdata}",')
        print(f'    "bundle_sha256": "{digest}",')
        print(f'    "count": {len(roots)},')
    else:
        if digest != SOURCE["bundle_sha256"]:
            raise SystemExit(f"gen_roots: {bundle_path} has SHA-256 {digest}, not the recorded "
                             f"{SOURCE['bundle_sha256']} (use --new to move to a new bundle)")
        assert when == SOURCE["mozilla_date"] and certdata == SOURCE["certdata_sha256"], (when, certdata)
        assert len(roots) == SOURCE["count"], len(roots)
    blob = b"".join(der for _, der in roots)
    man = [
        "# The TLS root store compiled into milkOS (tls::RootStore::mozilla), made by",
        "# scripts/gen_roots.py. Mozilla's NSS roots with the websites trust bit; data under",
        "# the MPL 2.0 (see NOTICE.txt here).",
        f"# source: {SOURCE['what']}",
        f"# Mozilla certdata as of: {when}",
        f"# certdata.txt SHA-256: {certdata}",
        f"# bundle SHA-256: {digest}",
        f"# roots: {len(roots)}, mozilla-roots.der: {len(blob)} bytes, SHA-256 {hashlib.sha256(blob).hexdigest()}",
        "# per root: SHA-256 fingerprint of the DER, key, label",
    ]
    for label, der in roots:
        man.append(f"{hashlib.sha256(der).hexdigest()} {describe_key(der):<11} {label}")
    return blob, "\n".join(man) + "\n"


def main():
    args = sys.argv[1:]
    if args and args[0] == "--check":
        blob, man = build(SOURCE["path"])
        ok = OUT_DER.read_bytes() == blob and OUT_TXT.read_text() == man
        print("gen_roots: committed files match the bundle" if ok else "gen_roots: committed files differ")
        sys.exit(0 if ok else 1)
    accept = False
    if args and args[0] == "--new":
        accept = True
        args = args[1:]
    path = args[0] if args else SOURCE["path"]
    blob, man = build(path, accept)
    OUT_DER.write_bytes(blob)
    OUT_TXT.write_text(man)
    print(f"gen_roots: {OUT_DER.relative_to(REPO)} ({len(blob)} bytes), {OUT_TXT.relative_to(REPO)}")


if __name__ == "__main__":
    main()
