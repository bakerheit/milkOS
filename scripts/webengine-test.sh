#!/usr/bin/env bash
# Unit tests for user/webengine on the Mac itself (the Browser's engine: HTML5 tokenizer and
# tree builder, CSS parsing / selectors / cascade / media queries / var(), block, inline,
# float, table, flex and positioned layout, the display list, forms, hit testing and find;
# the fixture pages, the ~1 MB generated page and hostile-input fuzzing). Prints
# "webengine-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" webengine webengine-unit
