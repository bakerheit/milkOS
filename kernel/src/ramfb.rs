//! ramfb: a simple framebuffer that qemu scans out of guest RAM. We tell it where the buffer is
//! (and its size and pixel format) by writing a `RAMFBCfg` to the fw_cfg file "etc/ramfb".

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::fwcfg;
use crate::gfx::Surface;

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 768;
const BPP: usize = 4;

/// DRM fourcc 'XR24' = XRGB8888: little-endian u32 `0x00RRGGBB`.
const FOURCC_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// Set once qemu has accepted the configuration.
static LIVE: AtomicBool = AtomicBool::new(false);

/// The framebuffer's kernel address (0 before [`init`]) and size. Its frames come from the
/// frame allocator, and only when ramfb is the display: with virtio-gpu nothing is taken.
static MODE_PTR: AtomicUsize = AtomicUsize::new(0);
static MODE_W: AtomicUsize = AtomicUsize::new(WIDTH);
static MODE_H: AtomicUsize = AtomicUsize::new(HEIGHT);
/// The frames of the current framebuffer (physical address, pages), for freeing.
static MODE_PA: AtomicUsize = AtomicUsize::new(0);
static MODE_PAGES: AtomicUsize = AtomicUsize::new(0);

/// The current mode: (width, height); the stride is the width.
pub fn size() -> (usize, usize) {
    (
        MODE_W.load(Ordering::Acquire),
        MODE_H.load(Ordering::Acquire),
    )
}

/// Switch the display to `w` x `h` (XRGB8888, stride = width): a new framebuffer from the
/// frame allocator, handed to qemu with a fresh `RAMFBCfg` (qemu re-creates its display
/// surface on every write). Returns the new surface; the previous framebuffer is
/// freed, so the caller must stop using its old surface first (`Surface` is just a view).
/// `None` (and nothing changed) if there's no ramfb or no memory. Thread context only.
pub fn set_mode(w: usize, h: usize) -> Option<Surface<'static>> {
    if !LIVE.load(Ordering::Acquire) {
        return None;
    }
    configure(w, h)
}

/// A fresh `w` x `h` framebuffer handed to qemu; the previous one (if any) freed.
fn configure(w: usize, h: usize) -> Option<Surface<'static>> {
    let file = fwcfg::find_file("etc/ramfb")?;
    let pages = (w * h * BPP).div_ceil(4096);
    let pa = crate::pmm::alloc_contiguous_zeroed_as(crate::pmm::Use::Display, pages, 1)?;
    let va = crate::mmu::phys_to_virt(pa);
    // Heap, not stack: fw_cfg writes by DMA from this address.
    let mut cfg = alloc::vec![0u8; 28];
    cfg[0..8].copy_from_slice(&(pa as u64).to_be_bytes());
    cfg[8..12].copy_from_slice(&FOURCC_XRGB8888.to_be_bytes());
    cfg[16..20].copy_from_slice(&(w as u32).to_be_bytes());
    cfg[20..24].copy_from_slice(&(h as u32).to_be_bytes());
    cfg[24..28].copy_from_slice(&((w * BPP) as u32).to_be_bytes());
    if (file.size as usize) < cfg.len() || !fwcfg::write_file(file, &mut cfg) {
        crate::pmm::free_contiguous(pa, pages);
        return None;
    }
    let old = (
        MODE_PA.swap(pa, Ordering::AcqRel),
        MODE_PAGES.swap(pages, Ordering::AcqRel),
    );
    MODE_W.store(w, Ordering::Release);
    MODE_H.store(h, Ordering::Release);
    MODE_PTR.store(va, Ordering::Release);
    if old.0 != 0 {
        crate::pmm::free_contiguous(old.0, old.1);
    }
    // SAFETY: `pages` fresh frames, mapped at `va`, owned by the display from now on.
    Some(unsafe { Surface::from_raw(va as *mut u32, w, h, w) })
}

/// Raw pointer to the live framebuffer ([`size`], stride = width), or `None` before
/// [`init`] succeeded. For overlays that draw next to the console (the mouse cursor); the
/// caller must serialise its writes with the console's (see `cursor.rs`).
pub fn framebuffer() -> Option<*mut u32> {
    LIVE.load(Ordering::Acquire)
        .then(|| MODE_PTR.load(Ordering::Acquire) as *mut u32)
}

/// Configure ramfb at 1024x768 XRGB8888 (a framebuffer from the frame allocator) and return
/// a surface over the buffer. `None` if there is no fw_cfg, no ramfb device (e.g. qemu was
/// run without `-device ramfb`) or no memory. Call once, after `mem::init`.
pub fn init() -> Option<Surface<'static>> {
    if !fwcfg::available() {
        return None;
    }
    let fb = configure(WIDTH, HEIGHT)?;
    LIVE.store(true, Ordering::Release);
    Some(fb)
}
