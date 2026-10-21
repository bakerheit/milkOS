#!/usr/bin/env python3
"""Check a Milk Drop eth1 capture (qemu filter-dump, pcap) for IPv6 discovery and MLD.

Exit 0 if the capture holds an MLDv2 report (ICMPv6 143 to ff02::16, hop limit 1, behind a
hop-by-hop header with a router alert) whose records include ff02::fb, and a Milk Drop
announcement sent to [ff02::fb]:5359 after it. Python stdlib only; reads the one file it
is given. Used by scripts/drop-test.sh.
"""
import ipaddress
import struct
import sys

MDNS6 = ipaddress.IPv6Address("ff02::fb")
MLD_ROUTERS = ipaddress.IPv6Address("ff02::16")


def frames(path):
    d = open(path, "rb").read()
    e = "<" if struct.unpack("<I", d[:4])[0] in (0xA1B2C3D4, 0xA1B23C4D) else ">"
    off = 24
    while off + 16 <= len(d):
        _, _, incl, _ = struct.unpack(e + "IIII", d[off : off + 16])
        yield d[off + 16 : off + 16 + incl]
        off += 16 + incl


def main():
    joined_at = None
    announce_at = None
    for n, f in enumerate(frames(sys.argv[1])):
        if len(f) < 54 or f[12:14] != b"\x86\xdd":
            continue
        ip = f[14:]
        nh, hop = ip[6], ip[7]
        dst = ipaddress.IPv6Address(ip[24:40])
        p = ip[40:]
        if nh == 0 and dst == MLD_ROUTERS and hop == 1 and len(p) >= 16:
            ra = p[2:6] == b"\x05\x02\x00\x00"
            m = p[8:]
            if p[0] == 58 and ra and m[0] == 143:
                nrec = struct.unpack(">H", m[6:8])[0]
                for i in range(nrec):
                    r = m[8 + 20 * i : 28 + 20 * i]
                    if len(r) == 20 and ipaddress.IPv6Address(r[4:]) == MDNS6 and r[0] == 4:
                        joined_at = n if joined_at is None else joined_at
        if nh == 17 and dst == MDNS6 and struct.unpack(">H", p[2:4])[0] == 5359:
            if b"ANNOUNCE name=" in p and announce_at is None:
                announce_at = n
    print(f"pcap_v6: MLDv2 join of ff02::fb at frame {joined_at}, first announcement to [ff02::fb]:5359 at frame {announce_at}")
    ok = joined_at is not None and announce_at is not None and joined_at < announce_at
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
