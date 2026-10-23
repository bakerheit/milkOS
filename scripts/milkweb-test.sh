#!/usr/bin/env bash
# Host tests of the milkweb crate (<repo>/milkweb) on the Mac itself, no VM and no network:
# the HTTP/1.1 client against scripted fake servers (Content-Length, chunked, gzip/deflate,
# redirect chains, loops and the limit of 10, https refused before connecting and the TLS hook,
# size and time limits, malformed answers), DNS messages (A/AAAA/CNAME, compression pointers,
# hostile and truncated input, TTLs, the cache, TCP after a truncated UDP answer, CNAME chains
# across questions), gzip members, URLs and the Browser's HTML subset (entities, charsets).
# Prints "milkweb-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" milkweb milkweb-unit
