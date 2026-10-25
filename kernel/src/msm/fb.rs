//! The Pixel 2's screen. The Android bootloader leaves its "splash" picture in a region of RAM
//! (`/reserved-memory/splash_region`, 36 MiB at 0x9d600000) and the display controller keeps
//! scanning it out, so drawing there is the whole display driver of stage 1, if the panel shows
//! what is written to RAM (a command-mode panel may need a refresh kick, [`kick`]).
//!
//! What is not known about that buffer is its pixel order and row length: the first guess is
//! 1080x1920, 32 bits, `B, G, R, x` in memory, rows packed (4320 bytes); `msm.fmt=`,
//! `msm.stride=` and `msm.size=` change the guess. The colour pattern ([`draw_pattern`]) exists so
//! a person can tell which guess is right. The mapping is the ordinary cacheable RAM one, so every
//! write is followed by a cache clean to the point of coherency ([`clean`]): the display controller
//! reads memory, not the CPU's caches.
//!
//! The console draws in a RAM copy of the screen in the kernel's XRGB and [`present`] sends the
//! changed rows through `rpikit::msm::pack_rows`, which writes them in the guessed format.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::dtb::Dtb;
use crate::fbcon::Console;
use crate::gfx::{DrawTarget, Surface};
use crate::mmu::phys_to_virt;
use crate::{font, kprintln, sink};
use rpikit::msm::{self, Fmt, Geometry, Opts};

/// Where Google's tree puts the splash region, used if the tree handed over does not have one.
pub const FALLBACK_PA: usize = 0x9d60_0000;
pub const FALLBACK_SIZE: usize = 0x240_0000;
/// Height in pixels of the green liveness band across the top of the console.
pub const BAND_H: usize = 6;

static PA: AtomicUsize = AtomicUsize::new(0);
static SIZE: AtomicUsize = AtomicUsize::new(0);
/// Geometry; zero width = not set up.
static W: AtomicUsize = AtomicUsize::new(0);
static H: AtomicUsize = AtomicUsize::new(0);
static STRIDE: AtomicUsize = AtomicUsize::new(0);
static FMT: AtomicU8 = AtomicU8::new(0);
static SCALE: AtomicUsize = AtomicUsize::new(2);

/// Where the framebuffer is and how the log should say we know.
pub struct Found {
    pub pa: usize,
    pub size: usize,
    pub source: &'static str,
}

/// The framebuffer's place: `msm.fb=`, else the tree's `splash_region`, else (on the phone) Google's
/// address. `None` only on the qemu test build, which has no such region.
pub fn locate(dtb: Option<&Dtb>, opts: &Opts) -> Option<Found> {
    if let Some((pa, size)) = opts.fb {
        return Some(Found {
            pa,
            size,
            source: "msm.fb on the command line",
        });
    }
    let mut hit = None;
    if let Some(d) = dtb {
        d.each_reserved_child(|name, base, size| {
            if hit.is_none() && name.starts_with(b"splash") && size != 0 {
                hit = Some((base as usize, size as usize));
            }
        });
    }
    if let Some((pa, size)) = hit {
        return Some(Found {
            pa,
            size,
            source: "splash_region in the device tree",
        });
    }
    if cfg!(feature = "msm-virt-test") {
        return None;
    }
    Some(Found {
        pa: FALLBACK_PA,
        size: FALLBACK_SIZE,
        source: "FALLBACK, the tree has no splash_region",
    })
}

/// Is `[pa, pa+size)` inside RAM the boot page tables map (so it can be drawn on before `mem::init`)?
pub fn boot_mapped(pa: usize, size: usize) -> bool {
    let map = crate::platform::CURRENT.boot_map;
    let ram = |a: usize| {
        map.get(a >> 30)
            .is_some_and(|b| *b == crate::platform::Block::Ram)
    };
    size != 0 && ram(pa) && ram(pa + size - 1)
}

/// Remember the region and the geometry guess. `size` 0 means "as much as the picture needs".
pub fn setup(found: &Found, opts: &Opts) -> Result<Geometry, &'static str> {
    let g = Geometry::new(opts.width, opts.height, opts.fmt, opts.stride);
    let size = if found.size == 0 { g.bytes_needed() } else { found.size };
    if !g.fits(size) {
        return Err("the picture does not fit the region");
    }
    PA.store(found.pa, Ordering::Release);
    SIZE.store(size, Ordering::Release);
    STRIDE.store(g.stride, Ordering::Release);
    FMT.store(Fmt::ALL.iter().position(|f| *f == g.fmt).unwrap_or(0) as u8, Ordering::Release);
    H.store(g.height, Ordering::Release);
    SCALE.store(opts.scale, Ordering::Release);
    W.store(g.width, Ordering::Release);
    Ok(g)
}

/// A framebuffer that is ordinary RAM from the allocator (the qemu test build's ramfb): XRGB, rows
/// packed.
#[cfg(feature = "msm-virt-test")]
pub fn setup_ram(va: usize, w: usize, h: usize, opts: &Opts) {
    PA.store(crate::mmu::virt_to_phys(va), Ordering::Release);
    SIZE.store(w * h * 4, Ordering::Release);
    STRIDE.store(w * 4, Ordering::Release);
    // qemu's ramfb is read as B, G, R, x; another 4-byte order shows with red and blue changed.
    let fmt = if opts.fmt.bytes() == 4 { opts.fmt } else { Fmt::Bgrx };
    FMT.store(Fmt::ALL.iter().position(|f| *f == fmt).unwrap_or(0) as u8, Ordering::Release);
    H.store(h, Ordering::Release);
    SCALE.store(opts.scale, Ordering::Release);
    W.store(w, Ordering::Release);
}

pub fn ready() -> bool {
    W.load(Ordering::Acquire) != 0
}

/// The geometry set up, if any.
pub fn geometry() -> Option<Geometry> {
    let w = W.load(Ordering::Acquire);
    if w == 0 {
        return None;
    }
    Some(Geometry {
        width: w,
        height: H.load(Ordering::Acquire),
        stride: STRIDE.load(Ordering::Acquire),
        fmt: Fmt::ALL[FMT.load(Ordering::Acquire) as usize],
    })
}

pub fn region() -> (usize, usize) {
    (PA.load(Ordering::Acquire), SIZE.load(Ordering::Acquire))
}

/// The framebuffer's bytes.
fn bytes() -> &'static mut [u8] {
    let (pa, size) = region();
    // SAFETY: the region is RAM mapped for the kernel (the boot tables, then the final ones: it is
    // not carved out), reserved from the allocator ([`reserve_frames`]), and only the code in this
    // file draws on it, one thread at a time (stage 1 has one CPU and no threads).
    unsafe { core::slice::from_raw_parts_mut(phys_to_virt(pa) as *mut u8, size) }
}

/// Write the cache lines over bytes `[lo, hi)` of the framebuffer back to memory, where the
/// display controller reads.
pub fn clean(lo: usize, hi: usize) {
    if hi <= lo {
        return;
    }
    let ctr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4usize << ((ctr >> 16) & 0xf);
    let base = phys_to_virt(region().0);
    let mut a = (base + lo) & !(line - 1);
    let end = base + hi;
    while a < end {
        // SAFETY: cache maintenance by address on mapped memory.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: barrier.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// The colour pattern, drawn straight into the framebuffer: the first thing the image does with
/// the screen, before there is a memory allocator.
pub fn draw_pattern() {
    let Some(g) = geometry() else { return };
    let (lo, hi) = msm::fill_pattern(&g, bytes());
    clean(lo, hi);
}

/// Pixels of a label's plate: 6 characters at scale 6, with padding.
const LABEL_W: usize = 6 * 8 * 6 + 32;
const LABEL_H: usize = 16 * 6 + 32;
static mut LABEL: [u32; LABEL_W * LABEL_H] = [0; LABEL_W * LABEL_H];

/// The block names on black plates in the middle of each block (white letters on black are the
/// same in every pixel order a guess can get wrong), and a caption row between the blocks.
pub fn draw_labels() {
    let Some(g) = geometry() else { return };
    let bpp = g.fmt.bytes();
    let dst = bytes();
    // SAFETY: the only user of LABEL, one thread (see `bytes`).
    let label = unsafe { &mut *core::ptr::addr_of_mut!(LABEL) };
    let mut hi_all = 0;
    let mut lo_all = usize::MAX;
    for b in msm::blocks(g.width, g.height) {
        let w = b.name.len() * 8 * 6 + 32;
        let mut s = Surface::new(&mut label[..], LABEL_W, LABEL_H, LABEL_W);
        s.clear(msm::BLACK);
        font::draw_str(&mut s, 16, 16, b.name, msm::WHITE, None, 6);
        let (x0, y0) = (b.x + (b.w.saturating_sub(w)) / 2, b.y + (b.h.saturating_sub(LABEL_H)) / 2);
        for y in 0..LABEL_H {
            for x in 0..w {
                let at = (y0 + y) * g.stride + (x0 + x) * bpp;
                if at + bpp > dst.len() || x0 + x >= g.width || y0 + y >= g.height {
                    continue;
                }
                g.fmt.put(&mut dst[at..], label[y * LABEL_W + x]);
            }
        }
        lo_all = lo_all.min(y0 * g.stride);
        hi_all = hi_all.max((y0 + LABEL_H) * g.stride);
    }
    clean(lo_all, hi_all.min(dst.len()));
}

/// Send rows `[lo, hi)` of the console's RAM copy to the screen in its pixel order.
pub fn present(shadow: &Surface<'static>, lo: usize, hi: usize) {
    let Some(g) = geometry() else { return };
    let (a, b) = msm::pack_rows(&g, bytes(), shadow.pixels(), shadow.stride(), lo, hi);
    clean(a, b);
}

/// Keep the allocator away from the framebuffer.
pub fn reserve_frames() {
    let (pa, size) = region();
    if size != 0 {
        crate::pmm::reserve(pa, size);
    }
}

/// The text console on the screen: a RAM copy drawn by `fbcon`, sent through [`present`].
pub fn console() -> Option<Console> {
    let g = geometry()?;
    let shadow = crate::display::console_shadow(g.width, g.height)?;
    Some(Console::with_present(shadow, SCALE.load(Ordering::Acquire), present))
}

/// The green band across the top of the screen with a white marker at `tick`, so a stopped
/// machine looks different from a running one.
pub fn band(c: &mut Console, tick: usize) {
    let (w, _) = geometry().map_or((0, 0), |g| (g.width, g.height));
    c.paint(0, BAND_H, |s| {
        s.fill_rect(0, 0, w as i32, BAND_H as i32, 0x0030_d060);
        let marker = 48usize;
        let x = if w > marker { (tick * 37) % (w - marker) } else { 0 };
        s.fill_rect(x as i32, 0, marker as i32, BAND_H as i32, msm::WHITE);
    });
}

/// The crash screen: the end of the kernel log as a full-screen text console. If the boot console
/// is still up it has the text already, so its last lines are flushed and it is left as it is.
pub fn crash_screen(tail: usize) -> bool {
    if !ready() {
        return false;
    }
    if sink::screen_installed() {
        sink::flush_screen();
        return true;
    }
    let Some(mut c) = console() else { return false };
    crate::klog::tail(tail, |bytes| {
        for chunk in bytes.utf8_chunks() {
            c.write_str(chunk.valid());
        }
    });
    c.write_str("\n\x1b[1;31mmilkOS stopped.\x1b[0m Photograph this screen.\n");
    c.flush();
    true
}

// ---- the refresh kick -------------------------------------------------------------------

/// MDSS control path registers (legacy `mdss_mdp`, offsets from the CTL block).
const CTL_TOP: usize = 0x14;
const CTL_FLUSH: usize = 0x18;
const CTL_START: usize = 0x1c;

/// Where CTL `index` is: the MDP node's first `reg` plus the `qcom,mdss-ctl-off` entry.
fn ctl_base(d: &Dtb, index: usize) -> Option<usize> {
    let mut reg = [(0u64, 0u64); 2];
    if d.node_reg(b"qcom,mdss_mdp@", &mut reg) == 0 {
        return None;
    }
    let offs = d.find_prefix(b"qcom,mdss_mdp@", b"qcom,mdss-ctl-off")?;
    let at = index.checked_mul(4)?;
    let off = u32::from_be_bytes(offs.get(at..at + 4)?.try_into().ok()?) as u64;
    let pa = reg[0].0 + off;
    let (lo, hi) = crate::platform::CURRENT.device.first().copied()?;
    (pa >= lo as u64 && pa + 0x40 <= hi as u64).then_some(pa as usize)
}

/// What `msm.kick=ctlN` does, once at the start: look at the control path, log it.
pub fn kick_probe(d: &Dtb, index: usize) -> Option<usize> {
    let Some(base) = ctl_base(d, index) else {
        kprintln!("kick: no usable ctl{index} in the tree (qcom,mdss_mdp reg, qcom,mdss-ctl-off)");
        return None;
    };
    // SAFETY: Device memory in the first GiB, addresses from the tree; reads only.
    let (top, flush) = unsafe {
        (
            core::ptr::read_volatile(phys_to_virt(base + CTL_TOP) as *const u32),
            core::ptr::read_volatile(phys_to_virt(base + CTL_FLUSH) as *const u32),
        )
    };
    kprintln!("kick: ctl{index} at {base:#x}, TOP {top:#x}, FLUSH {flush:#x}");
    Some(base)
}

fn rd(pa: usize) -> u32 {
    // SAFETY: Device memory in the first GiB, address checked by the caller; a read.
    unsafe { core::ptr::read_volatile(phys_to_virt(pa) as *const u32) }
}

/// A u32 array property of the display controller's node (`qcom,mdss-intf-off`...), entry `i`.
fn mdp_off(d: &Dtb, name: &[u8], i: usize) -> Option<usize> {
    let v = d.find_prefix(b"qcom,mdss_mdp@", name)?;
    let b = v.get(i * 4..i * 4 + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// `msm.mdp=dump`: read (never write) the control path and interface registers of the display
/// controller and log them: is a timing engine running (a panel driven in video mode), what is
/// the frame counter doing (a second reading a tenth of a second later), which CTL was
/// flushed last. Reads of a block whose clocks are off could hang the phone: it is opt-in.
pub fn mdp_dump(d: &Dtb, freq: u64) {
    let mut reg = [(0u64, 0u64); 2];
    if d.node_reg(b"qcom,mdss_mdp@", &mut reg) == 0 {
        kprintln!("mdp: no qcom,mdss_mdp node with a reg in the tree");
        return;
    }
    let base = reg[0].0 as usize;
    let (lo, hi) = crate::platform::CURRENT.device.first().copied().unwrap_or((0, 0));
    if base < lo || base + reg[0].1 as usize > hi {
        kprintln!("mdp: {base:#x} is outside the mapped device range");
        return;
    }
    kprintln!("mdp: base {base:#x}+{:#x}", reg[0].1);
    for i in 0..5 {
        let Some(off) = mdp_off(d, b"qcom,mdss-ctl-off", i) else { break };
        if off + 0x20 > reg[0].1 as usize {
            continue;
        }
        kprintln!(
            "mdp: ctl{i} at +{off:#x}: TOP {:#x} FLUSH {:#x}",
            rd(base + off + CTL_TOP),
            rd(base + off + CTL_FLUSH)
        );
    }
    let mut frames = [(0usize, 0u32); 4];
    let mut n = 0;
    for i in 0..4 {
        let Some(off) = mdp_off(d, b"qcom,mdss-intf-off", i) else { break };
        if off + 0xb4 > reg[0].1 as usize {
            continue;
        }
        let a = base + off;
        kprintln!(
            "mdp: intf{i} at +{off:#x}: EN {:#x} CONFIG {:#x} HSYNC {:#x} VSYNC_PERIOD {:#x} FRAMECNT {:#x}",
            rd(a),
            rd(a + 4),
            rd(a + 8),
            rd(a + 12),
            rd(a + 0xac)
        );
        frames[n] = (a, rd(a + 0xac));
        n += 1;
    }
    let t = crate::timer::counter() + freq / 10;
    while crate::timer::counter() < t {
        core::hint::spin_loop();
    }
    for (i, (a, before)) in frames[..n].iter().enumerate() {
        kprintln!("mdp: intf slot {i}: frame counter {before:#x} -> {:#x} in 0.1 s", rd(a + 0xac));
    }
}

/// Start the control path once more: a command-mode panel shows a frame only when told to.
pub fn kick(base: usize) {
    // SAFETY: as `kick_probe`; the one register written is CTL_START.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + CTL_START) as *mut u32, 1) };
}
