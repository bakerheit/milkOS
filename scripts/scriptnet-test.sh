#!/usr/bin/env bash
# Unit tests for user/scriptnet on the Mac itself (a page's fetch and XMLHttpRequest requests,
# run through the network policy and milkweb's HTTP client against a fake web: methods and
# bodies on the wire, same-origin, CORS and preflight, credentials, redirects hop by hop, no
# third-party cookies, private-network and mixed-content refusals, limits). Prints
# "scriptnet-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" scriptnet scriptnet-unit
