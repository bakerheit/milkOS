# milkOS

A hobby operating system for aarch64, written in Rust, run in qemu. Inspired by Linux (kernel
design, POSIX-ish userland) and macOS (the desktop and its feel).

## Run it
```
scripts/run.sh            # window + serial on the terminal
scripts/run.sh headless   # serial only
```
Needs Rust nightly via rustup (`brew install rustup`), and `qemu-system-aarch64`.

## Layout
- `kernel/`: the kernel crate (`no_std`). Entry is `kernel/src/boot.S`, then `kmain` in `main.rs`.
- `scripts/`: run and test helpers.
- `docs/`: design notes.
