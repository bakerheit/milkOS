#!/bin/bash
# dev helper (not committed)
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd /Users/andrewbaker/workspace/milkOS/.claude/worktrees/js-modules/user && cargo test -p jsengine --lib --target aarch64-apple-darwin --target-dir ../target/host-tests "$@" 2>&1
