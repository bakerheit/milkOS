//! ramfb: a simple framebuffer that qemu scans out of guest RAM. We tell it where the buffer is
//! (and its size and pixel format) by writing a `RAMFBCfg` to the fw_cfg file "etc/ramfb".

use crate::fwcfg;
use crate::gfx::Surface;

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 768;
const BPP: usize = 4;

/// DRM fourcc 'XR24' = XRGB8888: little-endian u32 `0x00RRGGBB`.
const FOURCC_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// The framebuffer memory: 3 MiB, page aligned, in .bss for now.
///
/// TODO(mem): once the frame allocator exists, allocate this from it (contiguous frames) and
/// pass the physical address to qemu. Nothing else depends on it being a static.
#[repr(C, align(4096))]
struct FbMem([u32; WIDTH * HEIGHT]);

static mut FB_MEM: FbMem = FbMem([0; WIDTH * HEIGHT]);

/// Configure ramfb at 1024x768 XRGB8888 and return a surface over the buffer.
/// `None` if there is no fw_cfg or no ramfb device (e.g. qemu was run without `-device ramfb`).
/// Call once.
pub fn init() -> Option<Surface<'static>> {
    if !fwcfg::available() {
        return None;
    }
    let file = fwcfg::find_file("etc/ramfb")?;
    // The surface returned below is the only Rust access to the buffer (init is called once).
    let mem = (&raw mut FB_MEM).cast::<u32>();

    // RAMFBCfg: addr u64, fourcc u32, flags u32, width u32, height u32, stride u32; all big endian.
    let mut cfg = [0u8; 28];
    cfg[0..8].copy_from_slice(&(mem as u64).to_be_bytes()); // physical == virtual with the MMU off
    cfg[8..12].copy_from_slice(&FOURCC_XRGB8888.to_be_bytes());
    cfg[12..16].copy_from_slice(&0u32.to_be_bytes());
    cfg[16..20].copy_from_slice(&(WIDTH as u32).to_be_bytes());
    cfg[20..24].copy_from_slice(&(HEIGHT as u32).to_be_bytes());
    cfg[24..28].copy_from_slice(&((WIDTH * BPP) as u32).to_be_bytes());
    if (file.size as usize) < cfg.len() || !fwcfg::write_file(file, &mut cfg) {
        return None;
    }
    // SAFETY: mem is 4096-aligned static storage of WIDTH*HEIGHT pixels.
    Some(unsafe { Surface::from_raw(mem, WIDTH, HEIGHT, WIDTH) })
}
