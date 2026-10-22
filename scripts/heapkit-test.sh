#!/usr/bin/env bash
# Unit tests for user/heapkit, the user-space heap allocator, on the Mac itself: random
# alloc/free/realloc against a model, memory reuse and return, fragmentation under churn, and
# threads. Prints "heapkit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" heapkit heapkit-unit
