#!/usr/bin/env python3
"""Print an /System/Settings/shadow password hash in milkOS's `$5m$` format (proc/login.rs,
user/rt/src/users.rs): h = SHA-256(salt + password), then rounds-1 times
h = SHA-256(h + salt + password).

    scripts/mkpasswd.py PASSWORD [ROUNDS]      (default 5000 rounds, random 16-byte salt)
"""
import hashlib, os, sys

def hash_pw(pw: bytes, salt: bytes, rounds: int) -> bytes:
    h = hashlib.sha256(salt + pw).digest()
    for _ in range(1, rounds):
        h = hashlib.sha256(h + salt + pw).digest()
    return h

if __name__ == "__main__":
    pw = sys.argv[1].encode()
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 5000
    salt = os.urandom(16)
    print(f"$5m${rounds}${salt.hex()}${hash_pw(pw, salt, rounds).hex()}")
