#!/usr/bin/env bash
# Unit tests for user/browserkit on the Mac itself (the Browser's rules and pixels: cookies,
# history, bookmarks, the address bar, forms, find, downloads' names, the built-in pages, the
# stand-in page renderer with its hostile-input tests, painting and zoom). Prints
# "browserkit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" browserkit browserkit-unit
