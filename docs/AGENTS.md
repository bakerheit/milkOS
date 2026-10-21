# Working on milkOS (for everyone, human or agent)

## Setup
- `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"` (rustup is keg-only). Nightly + `aarch64-unknown-none-softfloat`, pinned in `rust-toolchain.toml`.
- `cargo build` from the repo root. `scripts/run.sh` = window, `scripts/run.sh headless` = serial only,
  `scripts/screenshot.sh out.png [secs] [extra qemu args]` = headless boot + PNG + serial log next to it.
  `MILK_MONITOR=/tmp/x.sock scripts/run.sh headless` adds a qemu monitor socket (`info registers -a` on a hang).
  `scripts/serial-soak.sh [boots] [cmds]` = serial console soak (trickled commands; all must echo back).
- qemu: `-M virt -accel hvf -cpu host -smp 4`, GICv2, 512M. Kernel boots as a flat arm64 Image (`.bin`), so x0 = DTB.
- macOS has no `timeout`. Run qemu in the background, sleep, read, then kill **that exact PID**. Never broad `pkill`.

## Git
- Each agent works only in its own worktree, `.claude/worktrees/<name>`, on branch `<name>`. Never touch `main`
  or other worktrees. The lead merges. Don't merge `main` into your branch unless the lead asks.
- Commit often with clear messages. End every commit message with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Never push (there is no remote), never force anything, never `--no-verify`, never bare `git stash`.
- Keep `cargo build` and `cargo clippy` warning-free, run `cargo fmt`.
- Keep `kernel/src/main.rs` edits minimal (mod lines, init calls). Put real code in your own files.

## Kernel facts (as of main after the proc merge)
- Boot order in `kmain`: `exceptions::init` → `mem::init` (MMU + caches on) → `display::init` → `gic::init` →
  `timer::init` → `uart::init` → virtio → `sync::irq_enable` → self-tests → `fs::init` → `sched::init` →
  `smp::init` (CPUs 1-3 via PSCI) → sched self-test → `proc::init` → `proc::start_init` (PID 1 = `/bin/init`, which runs `msh` on the console).
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
  (`proc/caps.rs`, `/bin/sandbox`, model in `docs/security.md`) exist; caps default to "everything", so existing
  code is unaffected, but new device opens or syscalls that touch files/net/audio/clipboard/windows should check them.
- Locks: `sync::SpinLock<T>` (masks IRQs, really spins across CPUs, panics on same-CPU re-entry), `sync::IrqGuard`,
  `sync::without_irqs`, `sched::Mutex` for anything that may sleep.
- Interrupts: `gic::register(intid, handler)`; `gic::spi(n)`, `gic::ppi(n)`. Handlers get `&mut TrapFrame`, run
  with IRQs masked: no blocking.
- Time: `timer::now_ns()`, `timer::ticks()` (100 Hz), `timer::set_tick_hook`, `timer::set_deadline`. Use the virtual
  counter only (CNTP traps under HVF).
- Exceptions: `exceptions::set_syscall_handler`, `set_user_fault_handler`, `restore_frame(*const TrapFrame) -> !`.
- Display: `gfx::{DrawTarget, Surface, Rect, DirtyList}`, rounded rects, alpha, `font::draw_str_alpha`.
  `kprintln!` goes to UART + the screen console.

## UI
- Spec: `docs/ui-concepts/SPEC.md`, mockup `docs/ui-concepts/milkos-1.0.html` (the UI design). Andrew picked
  01 Whole Milk, 03 Skim, 06 Today, 07 Spaces, 10 Glass. Follow the spec numbers; ask the lead if one blocks you.
- Every window has its own offscreen surface. Nothing but the compositor writes to the framebuffer.

## Verifying
- Prove it in qemu. For UI work, screenshot and actually look at the PNG. Paste boot logs in reports.
