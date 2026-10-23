#!/usr/bin/env bash
# Unit tests for user/jsengine on the Mac itself (the Browser's JavaScript engine: tokenizer,
# parser, compiler, VM, GC, builtins and the embedding API; conformance tests written from the
# spec, golden tests, and fuzzing of the tokenizer, parser and VM with random and mutated
# scripts). Prints "jsengine-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" jsengine jsengine-unit
