//! The Android boot image header, version 0 (what the Pixel 2's bootloader reads for `fastboot
//! boot`): the layout, building and parsing it, and where the pages of the image go.
//! `scripts/android-bootimg.py` is the tool that builds a whole image; this is the same layout
//! in Rust so the two can be checked against each other (and against the header of a stock
//! image) by host tests.
//!
//! ```text
//!    0  magic "ANDROID!"            8 bytes
//!    8  kernel_size                 u32 (everything after the header, little-endian)
//!   12  kernel_addr                 u32
//!   16  ramdisk_size   20 ramdisk_addr
//!   24  second_size    28 second_addr
//!   32  tags_addr                   u32
//!   36  page_size                   u32
//!   40  header_version              u32 (0)
//!   44  os_version                  u32 (A.B.C << 14 | year-2000 << 4 | month << 0 style, opaque here)
//!   48  name                        16 bytes
//!   64  cmdline                     512 bytes, NUL terminated
//!  576  id                          32 bytes (SHA-1 of the contents, zero padded)
//!  608  extra_cmdline               1024 bytes
//! ```
//! The kernel starts at the next multiple of `page_size` after the header, the ramdisk at the
//! next page after the kernel, the second stage after the ramdisk.

pub const MAGIC: &[u8; 8] = b"ANDROID!";
pub const HEADER_SIZE: usize = 1632;
pub const CMDLINE_SIZE: usize = 512;
pub const EXTRA_CMDLINE_SIZE: usize = 1024;

/// The fields of a version 0 header.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Header {
    pub kernel_size: u32,
    pub kernel_addr: u32,
    pub ramdisk_size: u32,
    pub ramdisk_addr: u32,
    pub second_size: u32,
    pub second_addr: u32,
    pub tags_addr: u32,
    pub page_size: u32,
    pub header_version: u32,
    pub os_version: u32,
    pub name: [u8; 16],
    pub cmdline: [u8; CMDLINE_SIZE],
    pub id: [u8; 32],
    pub extra_cmdline: [u8; EXTRA_CMDLINE_SIZE],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Fewer than [`HEADER_SIZE`] bytes.
    Short,
    /// Not "ANDROID!".
    BadMagic,
    /// A page size that is not a power of two between 2048 and 16 MiB.
    BadPageSize,
    /// A header version this code does not know (only 0).
    UnsupportedVersion(u32),
}

/// The defaults the Pixel 2's own image uses: base 0, kernel at +0x8000, ramdisk at +0x1000000,
/// second stage at +0xf00000, tags at +0x100, 4 KiB pages.
pub const KERNEL_ADDR: u32 = 0x0000_8000;
pub const RAMDISK_ADDR: u32 = 0x0100_0000;
pub const SECOND_ADDR: u32 = 0x00f0_0000;
pub const TAGS_ADDR: u32 = 0x0000_0100;
pub const PAGE_SIZE: u32 = 4096;

impl Header {
    /// A header with the stock addresses, the given section sizes and command line (cut at the
    /// 512 bytes of `cmdline` and 1024 of `extra_cmdline`, a NUL always kept after the last
    /// byte of each; the command line is split at a space where it can be).
    pub fn new(kernel_size: u32, ramdisk_size: u32, cmdline: &[u8]) -> Header {
        let (a, b) = split_cmdline(cmdline);
        let mut h = Header {
            kernel_size,
            kernel_addr: KERNEL_ADDR,
            ramdisk_size,
            ramdisk_addr: RAMDISK_ADDR,
            second_size: 0,
            second_addr: SECOND_ADDR,
            tags_addr: TAGS_ADDR,
            page_size: PAGE_SIZE,
            header_version: 0,
            os_version: 0,
            name: [0; 16],
            cmdline: [0; CMDLINE_SIZE],
            id: [0; 32],
            extra_cmdline: [0; EXTRA_CMDLINE_SIZE],
        };
        h.cmdline[..a.len()].copy_from_slice(a);
        h.extra_cmdline[..b.len()].copy_from_slice(b);
        h
    }

    /// The header as the bytes of the image's first [`HEADER_SIZE`].
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut o = [0u8; HEADER_SIZE];
        o[..8].copy_from_slice(MAGIC);
        let words = [
            self.kernel_size,
            self.kernel_addr,
            self.ramdisk_size,
            self.ramdisk_addr,
            self.second_size,
            self.second_addr,
            self.tags_addr,
            self.page_size,
            self.header_version,
            self.os_version,
        ];
        for (i, w) in words.iter().enumerate() {
            o[8 + i * 4..12 + i * 4].copy_from_slice(&w.to_le_bytes());
        }
        o[48..64].copy_from_slice(&self.name);
        o[64..576].copy_from_slice(&self.cmdline);
        o[576..608].copy_from_slice(&self.id);
        o[608..].copy_from_slice(&self.extra_cmdline);
        o
    }

    /// Read a header from the start of an image.
    pub fn decode(b: &[u8]) -> Result<Header, Error> {
        if b.len() < HEADER_SIZE {
            return Err(Error::Short);
        }
        if &b[..8] != MAGIC {
            return Err(Error::BadMagic);
        }
        let w = |i: usize| u32::from_le_bytes([b[8 + i * 4], b[9 + i * 4], b[10 + i * 4], b[11 + i * 4]]);
        let h = Header {
            kernel_size: w(0),
            kernel_addr: w(1),
            ramdisk_size: w(2),
            ramdisk_addr: w(3),
            second_size: w(4),
            second_addr: w(5),
            tags_addr: w(6),
            page_size: w(7),
            header_version: w(8),
            os_version: w(9),
            name: b[48..64].try_into().unwrap(),
            cmdline: b[64..576].try_into().unwrap(),
            id: b[576..608].try_into().unwrap(),
            extra_cmdline: b[608..HEADER_SIZE].try_into().unwrap(),
        };
        if !h.page_size.is_power_of_two() || !(2048..=(16 << 20)).contains(&h.page_size) {
            return Err(Error::BadPageSize);
        }
        if h.header_version != 0 {
            return Err(Error::UnsupportedVersion(h.header_version));
        }
        Ok(h)
    }

    /// The command line: `cmdline` up to its NUL, then `extra_cmdline` up to its NUL (written
    /// into `out`; returns the length, cut at `out.len()`).
    pub fn cmdline_into(&self, out: &mut [u8]) -> usize {
        let a = cstr(&self.cmdline);
        let b = cstr(&self.extra_cmdline);
        let mut n = 0;
        for &c in a.iter().chain(b) {
            if n < out.len() {
                out[n] = c;
                n += 1;
            }
        }
        n
    }

    /// File offsets of the kernel, the ramdisk and the second stage, and the total size of the
    /// image (every section padded to whole pages).
    pub fn layout(&self) -> Layout {
        let p = self.page_size as u64;
        let pages = |n: u32| (n as u64).div_ceil(p) * p;
        let kernel = p; // the header takes one page (it is 1632 bytes; a page is at least 2048)
        let ramdisk = kernel + pages(self.kernel_size);
        let second = ramdisk + pages(self.ramdisk_size);
        Layout {
            kernel,
            ramdisk,
            second,
            total: second + pages(self.second_size),
        }
    }
}

/// Where the sections of an image start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    pub kernel: u64,
    pub ramdisk: u64,
    pub second: u64,
    pub total: u64,
}

fn cstr(b: &[u8]) -> &[u8] {
    &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]
}

/// Cut a command line into what fits `cmdline` (511 bytes and a NUL) and the rest for
/// `extra_cmdline` (1023 bytes and a NUL), at a space when there is one in the last 64 bytes
/// of the first part. What does not fit in both is dropped.
pub fn split_cmdline(c: &[u8]) -> (&[u8], &[u8]) {
    let c = &c[..c.len().min(CMDLINE_SIZE - 1 + EXTRA_CMDLINE_SIZE - 1)];
    if c.len() < CMDLINE_SIZE {
        return (c, &[]);
    }
    let mut cut = CMDLINE_SIZE - 1;
    if let Some(sp) = c[cut.saturating_sub(64)..cut].iter().rposition(|&b| b == b' ') {
        cut = cut.saturating_sub(64) + sp;
    }
    let rest = c[cut..].strip_prefix(b" ").unwrap_or(&c[cut..]);
    (&c[..cut], rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::vec::Vec;

    /// The header of the Pixel 2's stock boot image, field by field (the values are facts about
    /// the layout; no byte of Google's file is in this repository).
    fn stock_like() -> Header {
        let mut h = Header::new(0x00e4_27aa, 0x009e_2830, b"androidboot.hardware=walleye androidboot.console=ttyMSM0 lpm_levels.sleep_disabled=1 user_debug=31 msm_rtb.filter=0x37 ehci-hcd.park=3 service_locator.enable=1 swiotlb=2048 firmware_class.path=/vendor/firmware loop.max_part=7 raid=noautodetect usbcore.autosuspend=7 androidboot.dtbo_idx=3 buildvariant=user");
        h.os_version = 0x1400_0143;
        h
    }

    #[test]
    fn the_header_has_the_stock_layout_byte_for_byte() {
        let b = stock_like().encode();
        assert_eq!(b.len(), 1632);
        assert_eq!(&b[..8], b"ANDROID!");
        let word = |off: usize| u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
        assert_eq!(word(8), 0x00e4_27aa); // kernel_size
        assert_eq!(word(12), 0x8000); // kernel_addr
        assert_eq!(word(16), 0x009e_2830); // ramdisk_size
        assert_eq!(word(20), 0x0100_0000); // ramdisk_addr
        assert_eq!(word(24), 0); // second_size
        assert_eq!(word(28), 0x00f0_0000); // second_addr
        assert_eq!(word(32), 0x100); // tags_addr
        assert_eq!(word(36), 0x1000); // page_size
        assert_eq!(word(40), 0); // header_version
        assert_eq!(word(44), 0x1400_0143); // os_version
        assert!(b[48..64].iter().all(|&c| c == 0)); // name
        assert!(b[64..].starts_with(b"androidboot.hardware=walleye "));
        assert_eq!(b[64 + 20], b'=');
        // The command line is NUL terminated inside its 512 bytes and the rest of it is zero.
        let n = stock_like().cmdline.iter().position(|&c| c == 0).unwrap();
        assert_eq!(n, 306);
        assert!(b[64 + n..576].iter().all(|&c| c == 0));
        assert!(b[576..].iter().all(|&c| c == 0), "id and extra_cmdline are empty");
    }

    #[test]
    fn encode_then_decode_is_the_identity() {
        let h = stock_like();
        let b = h.encode();
        assert_eq!(Header::decode(&b), Ok(h.clone()));
        let mut long = Vec::new();
        for i in 0..80 {
            long.extend_from_slice(format!("opt{i}=value{i} ").as_bytes());
        }
        let h2 = Header::new(1, 2, &long);
        let mut buf = [0u8; 2000];
        let n = Header::decode(&h2.encode()).unwrap().cmdline_into(&mut buf);
        // What was cut at the space is put back with one space; what did not fit is gone.
        assert!(n > 700 && n <= long.len());
        assert!(buf[..n].starts_with(b"opt0=value0 opt1=value1 "));
        assert!(!buf[..n].contains(&0));
    }

    #[test]
    fn decode_refuses_what_is_not_a_version_0_header() {
        let b = stock_like().encode();
        assert_eq!(Header::decode(&b[..100]), Err(Error::Short));
        let mut bad = b;
        bad[0] = b'X';
        assert_eq!(Header::decode(&bad), Err(Error::BadMagic));
        let mut bad = b;
        bad[36..40].copy_from_slice(&3000u32.to_le_bytes());
        assert_eq!(Header::decode(&bad), Err(Error::BadPageSize));
        let mut bad = b;
        bad[36..40].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(Header::decode(&bad), Err(Error::BadPageSize));
        let mut bad = b;
        bad[40..44].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(Header::decode(&bad), Err(Error::UnsupportedVersion(2)));
    }

    #[test]
    fn sections_are_page_aligned_in_order() {
        let h = stock_like();
        let l = h.layout();
        assert_eq!(l.kernel, 0x1000);
        assert_eq!(l.ramdisk, 0x1000 + 0xe4_3000); // 0xe427aa rounded up to 4 KiB
        assert_eq!(l.second, l.ramdisk + 0x9e_3000);
        assert_eq!(l.total, l.second);
        let h = Header::new(0, 0, b"x");
        let l = h.layout();
        assert_eq!((l.kernel, l.ramdisk, l.second, l.total), (4096, 4096, 4096, 4096));
        let mut h = Header::new(4097, 1, b"");
        h.second_size = 5000;
        let l = h.layout();
        assert_eq!(l.ramdisk, 4096 + 8192);
        assert_eq!(l.second, l.ramdisk + 4096);
        assert_eq!(l.total, l.second + 8192);
    }

    #[test]
    fn long_command_lines_are_split_at_a_space() {
        let (a, b) = split_cmdline(b"a b c");
        assert_eq!((a, b), (&b"a b c"[..], &b""[..]));
        let mut c = Vec::new();
        while c.len() < 600 {
            c.extend_from_slice(b"word=12345 ");
        }
        let (a, b) = split_cmdline(&c);
        assert!(a.len() <= 511 && a.len() > 440);
        assert!(!a.ends_with(b" "));
        assert!(b.starts_with(b"word="));
        // No space to cut at: cut at 511.
        let solid = [b'x'; 700];
        let (a, b) = split_cmdline(&solid);
        assert_eq!((a.len(), b.len()), (511, 189));
        // Longer than both fields: the end is dropped.
        let huge = [b'y'; 4000];
        let (a, b) = split_cmdline(&huge);
        assert_eq!((a.len(), b.len()), (511, 1023));
    }
}
