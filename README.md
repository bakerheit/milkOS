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
- `scripts/`: run and test helpers. `scripts/screenshot.sh [out.png] [secs]` boots headless with ramfb
  and saves the screen as a PNG; `scripts/gen_font.py` regenerates `kernel/src/font_data.rs`.
- `docs/`: design notes.

## Display
ramfb (via qemu fw_cfg DMA) at 1024x768 XRGB8888. `gfx.rs` has the drawing primitives (`DrawTarget`,
`Surface`, rounded rects, alpha, dirty rects), `fbcon.rs` the text console, `sink.rs` mirrors `kprintln!`
to the screen, `splash.rs` is the boot splash. Without `-device ramfb` the kernel runs serial-only.

## Credits
The console font is [GNU Unifont](https://unifoundry.com/unifont/) 13.0.06 (8x16 glyphs, U+0020..U+00FF),
used under the SIL Open Font License 1.1. Licence text: `assets/fonts/UNIFONT-LICENSE.txt`.
