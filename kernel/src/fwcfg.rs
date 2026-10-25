//! qemu fw_cfg device (MMIO flavour, as on the `virt` machine).
//!
//! Registers (base 0x0902_0000): data at +0 (byte reads stream the selected item), selector at
//! +8 (16-bit, big endian), DMA address at +16 (big endian, high word first, the write to the low
//! word at +20 kicks off the transfer).
//!
//! Only the bits needed to find a named file and write to it are here.
//!
//! The Raspberry Pi has no fw_cfg. The boot options the tests and the launcher pass as
//! `opt/milkos/<name>` files come from the kernel command line there instead (the device tree's
//! `/chosen/bootargs`: cmdline.txt on the SD card, `-append` under qemu): `milk.<name>=<value>`
//! is the file `opt/milkos/<name>`; a value with spaces is quoted (`milk.net="internet=on dns=1.2.3.4:53"`).
//! [`find_file`] and [`read_file`] serve those, so callers need not know. Writing and the
//! `etc/ramfb` style device files do not exist there.

use core::ptr::{read_volatile, write_volatile};

const BASE: usize = crate::mmu::phys_to_virt(match crate::platform::CURRENT.fwcfg {
    Some(b) => b,
    None => 0,
});
const DATA: *mut u8 = BASE as *mut u8;
const SELECTOR: *mut u16 = (BASE + 8) as *mut u16;
const DMA_HI: *mut u32 = (BASE + 16) as *mut u32;
const DMA_LO: *mut u32 = (BASE + 20) as *mut u32;
const DMA_SIGNATURE: *const u64 = (BASE + 16) as *const u64;

const SEL_SIGNATURE: u16 = 0x0000;
const SEL_ID: u16 = 0x0001;
const SEL_FILE_DIR: u16 = 0x0019;

const ID_DMA: u32 = 1 << 1;

const DMA_ERROR: u32 = 1;
const DMA_READ: u32 = 1 << 1;
const DMA_SELECT: u32 = 1 << 3;
const DMA_WRITE: u32 = 1 << 4;

/// A fw_cfg file: the selector to use for it and its size in bytes.
#[derive(Clone, Copy)]
pub struct File {
    pub select: u16,
    pub size: u32,
}

/// The DMA access descriptor. All fields big endian. Must live in guest RAM.
#[repr(C, align(16))]
struct DmaAccess {
    control: u32,
    length: u32,
    address: u64,
}

static mut DMA_DESC: DmaAccess = DmaAccess {
    control: 0,
    length: 0,
    address: 0,
};

fn select(sel: u16) {
    // SAFETY: fw_cfg MMIO registers on qemu virt.
    unsafe { write_volatile(SELECTOR, sel.to_be()) };
}

fn read_bytes(buf: &mut [u8]) {
    for b in buf {
        // SAFETY: fw_cfg data register; each byte read advances the stream.
        *b = unsafe { read_volatile(DATA) };
    }
}

fn read_u32_be() -> u32 {
    let mut b = [0u8; 4];
    read_bytes(&mut b);
    u32::from_be_bytes(b)
}

/// The kernel command line of the Pi build (the device tree's `/chosen/bootargs`), as bytes.
fn bootargs() -> &'static [u8] {
    // SAFETY: the firmware's pointer, or 0; the blob is reserved (mem::init) and never freed.
    match unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) } {
        Some(d) => d.bootargs(),
        None => &[],
    }
}

/// The value of the boot option `milk.<name>` on the Pi's command line (None elsewhere, and when
/// the option is absent). The bytes live in the firmware's device tree, which is never freed.
#[cfg(feature = "raspi")]
pub fn option(name: &str) -> Option<&'static [u8]> {
    rpikit::cmdline::value(bootargs(), name)
}

/// True if a fw_cfg device is present and supports DMA (the Pi: always, the command line stands
/// in for it).
pub fn available() -> bool {
    if crate::platform::RASPI {
        return true;
    }
    select(SEL_SIGNATURE);
    let mut sig = [0u8; 4];
    read_bytes(&mut sig);
    if &sig != b"QEMU" {
        return false;
    }
    // The feature bitmap is a little-endian u32 (unlike everything else here).
    select(SEL_ID);
    let mut feat = [0u8; 4];
    read_bytes(&mut feat);
    if u32::from_le_bytes(feat) & ID_DMA == 0 {
        return false;
    }
    // SAFETY: reading the DMA signature register, which reads back "QEMU CFG" when DMA exists.
    let dma_sig = u64::from_be(unsafe { read_volatile(DMA_SIGNATURE) });
    dma_sig == u64::from_be_bytes(*b"QEMU CFG")
}

/// Look up a file in the fw_cfg directory by name (e.g. `"etc/ramfb"`).
pub fn find_file(name: &str) -> Option<File> {
    if crate::platform::RASPI {
        let (off, len) = rpikit::cmdline::find(bootargs(), rpikit::cmdline::option_name(name)?)?;
        return Some(File {
            select: u16::try_from(off).ok()?,
            size: len as u32,
        });
    }
    crate::platform::CURRENT.fwcfg?;
    select(SEL_FILE_DIR);
    let count = read_u32_be();
    for _ in 0..count {
        let size = read_u32_be();
        let mut sel = [0u8; 2];
        read_bytes(&mut sel);
        let mut reserved = [0u8; 2];
        read_bytes(&mut reserved);
        let mut raw = [0u8; 56];
        read_bytes(&mut raw);
        let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        if &raw[..len] == name.as_bytes() {
            return Some(File {
                select: u16::from_be_bytes(sel),
                size,
            });
        }
    }
    None
}

/// Run one DMA transfer and wait for it. Returns false if qemu reported an error.
fn dma(control: u32, buf: *mut u8, len: usize) -> bool {
    // SAFETY: single core, no interrupts touch fw_cfg; DMA_DESC is only used here.
    unsafe {
        let desc = &raw mut DMA_DESC;
        write_volatile(&raw mut (*desc).control, control.to_be());
        write_volatile(&raw mut (*desc).length, (len as u32).to_be());
        // The device wants physical addresses (works for buffers on kernel stacks too:
        // virt_to_phys knows the stack window; a stack is physically contiguous).
        let pa = crate::mmu::virt_to_phys(buf as usize) as u64;
        write_volatile(&raw mut (*desc).address, pa.to_be());
        core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        let d = crate::mmu::virt_to_phys(desc as usize) as u64;
        write_volatile(DMA_HI, ((d >> 32) as u32).to_be());
        write_volatile(DMA_LO, (d as u32).to_be());
        loop {
            let c = u32::from_be(read_volatile(&raw const (*desc).control));
            if c & DMA_ERROR != 0 {
                return false;
            }
            if c == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
}

/// Write `data` to the fw_cfg file `file` through the DMA interface.
pub fn write_file(file: File, data: &mut [u8]) -> bool {
    if crate::platform::RASPI {
        return false;
    }
    dma(
        DMA_SELECT | DMA_WRITE | ((file.select as u32) << 16),
        data.as_mut_ptr(),
        data.len(),
    )
}

/// Read up to `buf.len()` bytes of `file` through the DMA interface.
#[allow(dead_code)]
pub fn read_file(file: File, buf: &mut [u8]) -> bool {
    if crate::platform::RASPI {
        let args = bootargs();
        let (off, len) = (file.select as usize, file.size as usize);
        if off.checked_add(len).is_none_or(|e| e > args.len()) {
            return false;
        }
        let n = len.min(buf.len());
        buf[..n].copy_from_slice(&args[off..off + n]);
        return true;
    }
    dma(
        DMA_SELECT | DMA_READ | ((file.select as u32) << 16),
        buf.as_mut_ptr(),
        buf.len(),
    )
}
