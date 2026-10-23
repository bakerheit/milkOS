#!/usr/bin/env bash
# Unit tests for user/fontkit on the Mac itself: the baked font blobs (every one parses and
# validates), Latin-1 in every family at every size and weight, Greek/Cyrillic/Latin Extended
# and symbols coming from the family that has them, folding, the missing-glyph box, measuring,
# and truncated / mutated blobs. Prints "fontkit-unit: N passed". See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" fontkit fontkit-unit
