//! The display: a framebuffer the firmware allocates through the mailbox (rpikit::mailbox
//! `framebuffer_request`). We ask for a width x height x 32 bit surface in the byte order
//! `0x00RRGGBB` as a little-endian word, the format the whole desktop draws in, and take
//! the address, size and pitch from the firmware's answer. A pitch that is not width * 4 is
//! absorbed by growing the width to fill the row (the rest of the kernel treats the stride as
//! the width), and an answer that is not 32 bits per pixel in that order is refused: no
//! screen, serial only, rather than wrong colours.
//!
//! On a real board the buffer lives in the GPU's memory, outside the ARM's RAM, and is
//! mapped here as non-cacheable Normal memory (the scan-out reads RAM behind the caches;
//! unaligned and gathered stores stay legal, which Device memory would not allow). Under qemu
//! the buffer is inside the RAM we manage and the linear map already covers it; its frames
//! are reserved so nothing else gets them.
//!
//! The scan-out follows the memory continuously, so [`present`] and [`wait`] have nothing to
//! do, like ramfb.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rpikit::mailbox as msg;

use super::mailbox;
use crate::gfx::Surface;
use crate::kprintln;
use crate::mmu::{self, MapAttrs, MemType, PAGE_SIZE, phys_to_virt};

static LIVE: AtomicBool = AtomicBool::new(false);
static PTR: AtomicUsize = AtomicUsize::new(0);
static W: AtomicUsize = AtomicUsize::new(0);
static H: AtomicUsize = AtomicUsize::new(0);

/// What the firmware gave us for a request, or why it did not.
fn request(w: u32, h: u32) -> Result<msg::Framebuffer, mailbox::Error> {
    mailbox::transact(
        |buf| msg::framebuffer_request(buf, w, h, 32).map(|(_, tags)| tags),
        |buf, tags| msg::framebuffer_reply(buf, &tags),
    )
}

/// Make `[phys, phys + size)` addressable: already in the linear map (it is RAM we manage:
/// reserve the frames), or mapped here as non-cacheable Normal memory. Returns its kernel
/// virtual address.
fn map_buffer(phys: usize, size: usize) -> Option<usize> {
    let va = phys_to_virt(phys);
    let end = (phys + size).next_multiple_of(PAGE_SIZE);
    let start = phys & !(PAGE_SIZE - 1);
    let in_ram = mmu::with_kernel_space(|s| s.translate(phys_to_virt(start)).is_some());
    if in_ram {
        crate::pmm::reserve(start, end - start);
        return Some(va);
    }
    mmu::with_kernel_space(|s| {
        s.map(
            phys_to_virt(start),
            start,
            end - start,
            MapAttrs {
                mem: MemType::NormalNc,
                write: true,
                exec: false,
                user: false,
            },
            &mut mmu::PmmTables,
        )
    })
    .ok()?;
    Some(va)
}

fn configure(w: usize, h: usize) -> Option<Surface<'static>> {
    let (w, h) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    let mut fb = match request(w, h) {
        Ok(f) => f,
        Err(e) => {
            kprintln!("rpi fb: the firmware refused {w}x{h}: {e:?}");
            return None;
        }
    };
    if fb.pitch != fb.width * 4 {
        // Rows are padded: ask again with the padded width, so stride = width.
        let padded = fb.pitch / 4;
        kprintln!(
            "rpi fb: pitch {} is not {} * 4, asking for width {padded}",
            fb.pitch,
            fb.width
        );
        fb = request(padded, h).ok()?;
        if fb.pitch != fb.width * 4 {
            kprintln!("rpi fb: the pitch still does not match the width, no display");
            return None;
        }
    }
    if fb.order != 0 {
        kprintln!("rpi fb: the firmware uses RGB pixel order, which the desktop does not draw in, no display");
        return None;
    }
    let va = map_buffer(fb.phys as usize, fb.size as usize)?;
    PTR.store(va, Ordering::Release);
    W.store(fb.width as usize, Ordering::Release);
    H.store(fb.height as usize, Ordering::Release);
    kprintln!(
        "rpi fb: {}x{}x{} pitch {} at {:#x} ({} KiB)",
        fb.width,
        fb.height,
        fb.depth,
        fb.pitch,
        fb.phys,
        fb.size >> 10
    );
    // SAFETY: the firmware's buffer, mapped above, owned by the display from now on.
    Some(unsafe { Surface::from_raw(va as *mut u32, fb.width as usize, fb.height as usize, fb.width as usize) })
}

/// Allocate the framebuffer at `w` x `h` (or what the firmware makes of it). `None` without a
/// usable display.
pub fn init(w: usize, h: usize) -> Option<Surface<'static>> {
    let s = configure(w, h)?;
    LIVE.store(true, Ordering::Release);
    Some(s)
}

/// Switch mode: a new surface over the (re)allocated buffer. The caller stops using the old
/// surface first.
pub fn set_mode(w: usize, h: usize) -> Option<Surface<'static>> {
    if !LIVE.load(Ordering::Acquire) {
        return None;
    }
    configure(w, h)
}

pub fn size() -> (usize, usize) {
    (W.load(Ordering::Acquire), H.load(Ordering::Acquire))
}

pub fn framebuffer() -> Option<*mut u32> {
    LIVE.load(Ordering::Acquire)
        .then(|| PTR.load(Ordering::Acquire) as *mut u32)
}
