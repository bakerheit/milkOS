# Working on milkOS (for everyone, human or agent)

## Setup
- `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` (rustup is keg-only). Nightly + `aarch64-unknown-none-softfloat`, pinned in `rust-toolchain.toml`.
- `cargo build` from the repo root. `scripts/run.sh` = window, `scripts/run.sh headless` = serial only, both
  with the internet on: for anything automated use `MILK_OFFLINE=1 scripts/run.sh headless` (tests stay offline;
  check-all refuses an online boot, docs/networking.md "Internet"),
  `scripts/screenshot.sh out.png [secs] [extra qemu args]` = headless boot + PNG + serial log next to it.
  `MILK_MONITOR=/tmp/x.sock scripts/run.sh headless` adds a qemu monitor socket (`info registers -a` on a hang).
  `scripts/serial-soak.sh [boots] [cmds]` = serial console soak (trickled commands; all must echo back).
  `scripts/svc-test.sh` = milkd services (backoff, dependencies, `services` permissions), admin tickets (`utest auth`) + the journal across a reboot.
  `scripts/systools-test.sh` = `processes`, `services`, `system`, `users`, `admin` end to end in a VM; `scripts/syskit-test.sh` = their rules (user/syskit) on the Mac.
  `scripts/browser-test.sh` = the Browser (/Apps/browser, docs/browser.md) end to end against local test servers; `scripts/browserkit-test.sh` = its rules on the Mac;
  `scripts/memory-test.sh` = its memory (budgets that follow free memory, pressure, streaming; docs/browser.md "Memory").
  `scripts/milkfs-test.sh` = the file system's host tests (the `milkfs` crate on the Mac, seconds, no VM; docs/filesystem.md).
  `scripts/driver-crash-test.sh` = user-space drivers (rngd, soundd; docs/drivers.md) killed mid-use: restarted, audio keeps playing, no panic; `MILK_DRIVERS="rng=kernel sound=kernel"` for the in-kernel drivers only.
  `scripts/pi-run.sh` = the Raspberry Pi 4 kernel on qemu `-M raspi4b` (docs/raspberry-pi.md); `scripts/pi-build.sh` builds its image (feature
  `raspi`, target dir `target/pi`); `scripts/pi-qemu-test.sh` boots it five times (desktop, SD card, two RAM banks, both crash screens);
  `scripts/usb-test.sh` = USB keyboard and mouse through qemu's xHCI; `scripts/highmem-test.sh` = RAM above 4 GiB on virt;
  `scripts/rpikit-test.sh` = the Pi crate's host tests (device tree, mailbox, SD, PCI, USB, GENET).
  Daemons are milkd services (`/System/Settings/milk/services/*.service`, docs/processes.md); don't add hard-coded starts to init.
- qemu: `-M virt -accel hvf -cpu host -smp 4`, GICv2, 512M. Kernel boots as a flat arm64 Image (`.bin`), so x0 = DTB.
- Two boards, one source tree: `kernel/src/platform.rs` has the table (qemu virt, Raspberry Pi 4 behind `--features raspi`);
  board addresses go through `platform::CURRENT`, never literals. DMA memory comes from `dma::Buffer` / `pmm::alloc_dma_zeroed_as` (below
  the board's DMA limit, synced around hand-overs); physical addresses are 64-bit and RAM can be several banks. Pi drivers live in
  `kernel/src/rpi/`, pure parts in the `rpikit` crate (host tests).
- macOS has no `timeout`. Run qemu in the background, sleep, read, then kill **that exact PID**. Never broad `pkill`.

## Git
- Each agent works only in its own worktree, `.claude/worktrees/<name>`, on branch `<name>`. Never touch `main`
  or other worktrees. The lead merges. Don't merge `main` into your branch unless the lead asks.
- Commit often with clear messages, as `bakerheit <270044729+bakerheit@users.noreply.github.com>`
  (`git -c user.name=bakerheit -c user.email=270044729+bakerheit@users.noreply.github.com commit ...`). End every
  commit message with the `Co-Authored-By: Claude ... <noreply@anthropic.com>` line the lead gives you.
- The repo is public (github.com/bakerheit/milkOS). No people or team names, ticket ids, personal paths or e-mail
  addresses in files, commit messages or merge messages (`scripts/public-check.sh` enforces it).
- Never push (only the lead pushes main, fast-forward only, after the full check-all passes), never force anything,
  never `--no-verify`, never bare `git stash`. Releases and tags: docs/releasing.md.
- Keep `cargo build` and `cargo clippy` warning-free, run `cargo fmt`.
- `kernel/build.rs` fails every build if a `user/<crate>` program isn't listed in `initramfs.list`, so register a new
  crate in the same commit that adds it (or other people's test rebuilds break).
- Keep `kernel/src/main.rs` edits minimal (mod lines, init calls). Put real code in your own files.

## Kernel facts (as of main after the proc merge)
- Boot order in `kmain`: `exceptions::init` → `mem::init` (MMU + caches on) → `display::init` → `gic::init` →
  `timer::init` → `uart::init` → virtio → `sync::irq_enable` → self-tests → `fs::init` → `sched::init` →
  `smp::init` (CPUs 1-3 via PSCI) → sched self-test → `proc::init` → `proc::start_init` (PID 1 = `/System/Startup/init`, which runs `msh` on the console).
- Threads: `sched::spawn`/`spawn_prio`, `sleep_ms`, `sleep_until`, `yield_now`, `WaitQueue` (incl.
  `wait_until_timeout`), `sched::Mutex`. Never block in an IRQ handler or while holding a `SpinLock`.
- Ownership: `sched` owns `timer::set_tick_hook` and `timer::set_deadline` (don't call them elsewhere; use threads and
  sleeps). The console tty (`proc/console.rs`) owns `uart::read_byte`. Processes and syscalls: `docs/processes.md`.
  Files: `docs/filesystem.md`. User programs live in `user/` and are listed in `initramfs.list`.
- Memory: higher-half kernel. Kernel VA = `mmu::phys_to_virt(pa)` = `0xffff_0000_0000_0000 + pa`; TTBR0 is user
  space only. MMIO bases are `phys_to_virt(<pa>)`; anything a device reads (DMA, rings, fw_cfg) gets
  `mmu::virt_to_phys(ptr)`. Never hand a device a raw pointer cast. See `docs/memory.md`. Heap via `alloc` (Vec/Box/BTreeMap fine). DMA / big buffers:
  `pmm::alloc_contiguous_zeroed(npages, align_frames)`, free with `pmm::free_contiguous`. No allocation in IRQ handlers.
- SMP: milkOS runs on 4 CPUs by default (`MILK_SMP=1` for one). `percpu::id()` is the current CPU. Any shared
  state needs a real lock or atomics; "single core, plain load/store is fine" is no longer true. See `docs/smp.md`.
- Processes can be multi-threaded (clone CLONE_THREAD, futex, `user/rt` thread/Mutex/Condvar); freeing a
  multi-threaded process's page tables goes through `tlb::shootdown`. `chroot` and the capability sandbox
  (`proc/caps.rs`, `/System/Developer/sandbox`, model in `docs/security.md`) exist; caps default to "everything", so existing
  code is unaffected, but new device opens or syscalls that touch files/net/audio/clipboard/windows should check them.
- Locks: `sync::SpinLock<T>` (masks IRQs, really spins across CPUs, panics on same-CPU re-entry), `sync::IrqGuard`,
  `sync::without_irqs`, `sched::Mutex` for anything that may sleep.
- Interrupts: `gic::register(intid, handler)`; `gic::spi(n)`, `gic::ppi(n)`. Handlers get `&mut TrapFrame`, run
  with IRQs masked: no blocking.
- Time: `timer::now_ns()`, `timer::ticks()` (100 Hz), `timer::set_tick_hook`, `timer::set_deadline`. Use the virtual
  counter only (CNTP traps under HVF).
- Exceptions: `exceptions::set_syscall_handler`, `set_user_fault_handler`, `restore_frame(*const TrapFrame) -> !`.
- Display: `gfx::{DrawTarget, Surface, Rect, DirtyList}`, rounded rects, alpha, `font::draw_str_alpha` (Unifont cells).
  Desktop UI text goes through `sysfont::Font` (measure with `width`/`truncate`, draw with the painter's `text_*`);
  never assume 8 px a character outside monospace grids. User-space apps get the same fonts through
  `gui::text` / `appkit::text` (`Window::text` is the system font, `Window::mono` Unifont; docs/processes.md).
  `kprintln!` goes to UART + the screen console.

## UI
- Spec: `docs/ui-concepts/SPEC.md`, mockup `docs/ui-concepts/milkos-1.0.html` (the UI design). The project owner picked
  01 Whole Milk, 03 Skim, 06 Today, 07 Spaces, 10 Glass. Follow the spec numbers; ask the lead if one blocks you.
- Every window has its own offscreen surface. Nothing but the compositor writes to the framebuffer.

## Verifying
- `scripts/check-all.sh` runs every test script, eight at a time (private images and ports per test, time limits,
  one pass/fail table with the wall time, non-zero exit on any fail; about 2 minutes, `-j 1` for one at a time).
  `--smoke` is one test per area (run it on every change), `--full` adds the long soak/crash/stress runs,
  `--only REGEX` picks some, `--list` shows them. The default set gates a push: run it before asking the lead
  to merge. A pass on a clean tree is recorded, and `--if-needed` skips a set this exact tree already passed.
  A new test that can't share the machine gets mode `solo` in the table.
- VMs boot without the slow kernel self-tests (`opt/milkos/selftest`, kernel/src/selftest.rs);
  `scripts/selftest-boot.sh` runs them all, and `MILK_SELFTEST=full scripts/run.sh` boots with them.
- Disk: a test removes its own /tmp work folder when it passes and keeps it when it fails (`MILK_KEEP=1` or
  check-all `--keep` keeps it anyway). check-all refuses to start with under 5 GB free in /tmp.
- `scripts/public-check.sh` (also the first test in check-all) keeps the public repo clean: it fails on names from a
  local list (kept outside the repo, see the script header), e-mail addresses other than noreply ones, host home
  paths, key material and ticket ids, in the working tree and in commits not on origin/main. Run it before
  committing. Without the local list it skips the names rule and says so.
- The version is in `VERSION` and nowhere else by hand: never edit a version number (Cargo, docs, strings);
  `scripts/cut-release.sh` does the bump (docs/releasing.md) and `scripts/version-check.sh` (in the smoke set) fails
  if anything disagrees. Changing a format's version (milkfs, `.milkpkg`, streams, ...) means updating the table
  and the `format-constants` block in docs/versioning.md, a migration and a changelog line. Agents don't cut,
  tag or publish releases; the lead does. `scripts/ci-local.sh` runs the portable checks the GitHub workflow runs.
- Prove it in qemu. For UI work, screenshot and actually look at the PNG. Paste boot logs in reports.
