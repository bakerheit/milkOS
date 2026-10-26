//! A flattened device tree writer: the other half of [`crate::dtb`]'s reader. It builds a blob in
//! a buffer the caller gives it (no allocation), the way `dtc` lays one out: header, the memory
//! reservation block, the structure block, the strings block. `scripts/dtb_build.py` makes the
//! tree for the Pixel 2's bootloader from the same description; the tests of both check a small
//! tree byte for byte.
//!
//! ```ignore
//! let mut w = Writer::new(&mut out, &mut strings);
//! w.begin_node("")?;                       // the root
//! w.prop_u32("#address-cells", 2)?;
//! w.begin_node("memory")?;
//! w.prop_str("device_type", "memory")?;
//! w.end_node()?;
//! w.end_node()?;
//! let len = w.finish(&[])?;                // out[..len] is the blob
//! ```

const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_END: u32 = 9;
const HEADER: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The output or the strings buffer is full.
    Full,
    /// `end_node` without a node, `finish` with nodes still open, or a property outside a node.
    Structure,
}

pub struct Writer<'a> {
    out: &'a mut [u8],
    strings: &'a mut [u8],
    /// Bytes of `out` used by the structure block (which starts at `HEADER`; the reservation
    /// block is inserted by `finish`).
    st: usize,
    str_len: usize,
    depth: usize,
}

impl<'a> Writer<'a> {
    /// `out` receives the blob (the structure block is built in it, so it must be big enough
    /// for the whole blob); `strings` is scratch for the property names.
    pub fn new(out: &'a mut [u8], strings: &'a mut [u8]) -> Writer<'a> {
        Writer {
            out,
            strings,
            st: 0,
            str_len: 0,
            depth: 0,
        }
    }

    fn word(&mut self, v: u32) -> Result<(), Error> {
        self.bytes(&v.to_be_bytes())
    }

    fn bytes(&mut self, b: &[u8]) -> Result<(), Error> {
        // Leave room for the header and a worst-case reservation block (see `finish`).
        let at = RESERVED + self.st;
        let end = at.checked_add(b.len()).ok_or(Error::Full)?;
        if end > self.out.len() {
            return Err(Error::Full);
        }
        self.out[at..end].copy_from_slice(b);
        self.st += b.len();
        Ok(())
    }

    fn pad(&mut self) -> Result<(), Error> {
        while self.st % 4 != 0 {
            self.bytes(&[0])?;
        }
        Ok(())
    }

    /// Open a node (`""` for the root).
    pub fn begin_node(&mut self, name: &str) -> Result<(), Error> {
        self.word(FDT_BEGIN_NODE)?;
        self.bytes(name.as_bytes())?;
        self.bytes(&[0])?;
        self.pad()?;
        self.depth += 1;
        Ok(())
    }

    pub fn end_node(&mut self) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::Structure);
        }
        self.depth -= 1;
        self.word(FDT_END_NODE)
    }

    fn string_off(&mut self, name: &str) -> Result<u32, Error> {
        let n = name.as_bytes();
        // Reuse a name that is already there (whole string match).
        let mut i = 0;
        while i < self.str_len {
            let end = self.strings[i..self.str_len]
                .iter()
                .position(|&c| c == 0)
                .map_or(self.str_len, |p| i + p);
            if &self.strings[i..end] == n {
                return Ok(i as u32);
            }
            i = end + 1;
        }
        let at = self.str_len;
        if at + n.len() + 1 > self.strings.len() {
            return Err(Error::Full);
        }
        self.strings[at..at + n.len()].copy_from_slice(n);
        self.strings[at + n.len()] = 0;
        self.str_len = at + n.len() + 1;
        Ok(at as u32)
    }

    pub fn prop(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::Structure);
        }
        let off = self.string_off(name)?;
        self.word(FDT_PROP)?;
        self.word(data.len() as u32)?;
        self.word(off)?;
        self.bytes(data)?;
        self.pad()
    }

    pub fn prop_empty(&mut self, name: &str) -> Result<(), Error> {
        self.prop(name, &[])
    }

    pub fn prop_u32(&mut self, name: &str, v: u32) -> Result<(), Error> {
        self.prop(name, &v.to_be_bytes())
    }

    /// A list of 32-bit cells.
    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::Structure);
        }
        let off = self.string_off(name)?;
        self.word(FDT_PROP)?;
        self.word((cells.len() * 4) as u32)?;
        self.word(off)?;
        for &c in cells {
            self.word(c)?;
        }
        Ok(())
    }

    /// A 64-bit value as two cells.
    pub fn prop_u64(&mut self, name: &str, v: u64) -> Result<(), Error> {
        self.prop_cells(name, &[(v >> 32) as u32, v as u32])
    }

    /// A string, NUL terminated.
    pub fn prop_str(&mut self, name: &str, s: &str) -> Result<(), Error> {
        self.prop_strs(name, &[s])
    }

    /// A list of strings, each NUL terminated (`compatible`).
    pub fn prop_strs(&mut self, name: &str, list: &[&str]) -> Result<(), Error> {
        if self.depth == 0 {
            return Err(Error::Structure);
        }
        let off = self.string_off(name)?;
        let len: usize = list.iter().map(|s| s.len() + 1).sum();
        self.word(FDT_PROP)?;
        self.word(len as u32)?;
        self.word(off)?;
        for s in list {
            self.bytes(s.as_bytes())?;
            self.bytes(&[0])?;
        }
        self.pad()
    }

    /// Close the blob: header, the memory reservation block (`reserve` as `(address, size)`
    /// pairs, then the zero terminator), the structure block, the strings. Returns the blob's
    /// length (`out[..length]`).
    pub fn finish(mut self, reserve: &[(u64, u64)]) -> Result<usize, Error> {
        if self.depth != 0 {
            return Err(Error::Structure);
        }
        self.word(FDT_END)?;
        let rsv = (reserve.len() + 1) * 16;
        if HEADER + rsv > RESERVED {
            return Err(Error::Full);
        }
        let struct_off = HEADER + rsv;
        let strings_off = struct_off + self.st;
        let total = strings_off + self.str_len;
        if total > self.out.len() {
            return Err(Error::Full);
        }
        // Slide the structure block down from RESERVED to where it belongs, then the strings.
        self.out.copy_within(RESERVED..RESERVED + self.st, struct_off);
        self.out[strings_off..total].copy_from_slice(&self.strings[..self.str_len]);
        let mut p = HEADER;
        for &(a, s) in reserve {
            self.out[p..p + 8].copy_from_slice(&a.to_be_bytes());
            self.out[p + 8..p + 16].copy_from_slice(&s.to_be_bytes());
            p += 16;
        }
        self.out[p..p + 16].fill(0);
        let hdr = [
            FDT_MAGIC,
            total as u32,
            struct_off as u32,
            strings_off as u32,
            HEADER as u32, // off_mem_rsvmap
            17,            // version
            16,            // last_comp_version
            0,             // boot_cpuid_phys
            self.str_len as u32,
            self.st as u32,
        ];
        for (i, v) in hdr.iter().enumerate() {
            self.out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        Ok(total)
    }
}

/// Room kept in front of the structure block for the header and up to 63 reservations plus the
/// terminator while the structure is built (it is moved into place by `finish`).
const RESERVED: usize = HEADER + 64 * 16;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dtb::{Conduit, Dtb, EnableMethod, MemRange, MAX_BANKS};
    use std::vec;
    use std::vec::Vec;

    /// The tree both this test and `scripts/dtb_build_test.py` build and compare as bytes.
    pub fn small_tree() -> Vec<u8> {
        let mut out = vec![0u8; 4096];
        let mut strs = vec![0u8; 512];
        let mut w = Writer::new(&mut out, &mut strs);
        w.begin_node("").unwrap();
        w.prop_u32("#address-cells", 2).unwrap();
        w.prop_u32("#size-cells", 2).unwrap();
        w.prop_str("model", "test").unwrap();
        w.prop_strs("compatible", &["a,b", "c,d"]).unwrap();
        w.prop_cells("qcom,msm-id", &[0x124, 0x20001]).unwrap();
        w.begin_node("memory").unwrap();
        w.prop_str("device_type", "memory").unwrap();
        w.prop_cells("reg", &[0, 0x8000_0000, 0, 0x4000_0000]).unwrap();
        w.end_node().unwrap();
        w.begin_node("chosen").unwrap();
        w.prop_empty("flag").unwrap();
        w.end_node().unwrap();
        w.end_node().unwrap();
        let n = w.finish(&[(0x1000, 0x2000)]).unwrap();
        out.truncate(n);
        out
    }

    /// The same tree as hex, the golden bytes the Python builder is held to.
    pub const SMALL_TREE_HEX: &str = "d00dfeed0000015d0000004800000110000000280000001100000010000000000000004d000000c8000000000000100000000000000020000000000000000000000000000000000000000001000000000000000300000004000000000000000200000003000000040000000f0000000200000003000000050000001b7465737400000000000000030000000800000021612c6200632c640000000003000000080000002c0000012400020001000000016d656d6f727900000000000300000007000000386d656d6f7279000000000003000000100000004400000000800000000000000040000000000000020000000163686f73656e000000000003000000000000004800000002000000020000000923616464726573732d63656c6c73002373697a652d63656c6c73006d6f64656c00636f6d70617469626c650071636f6d2c6d736d2d6964006465766963655f747970650072656700666c616700";

    fn dtb(blob: &[u8]) -> Dtb {
        // The reader wants an 8-byte aligned address that stays put.
        let b = blob.to_vec().leak();
        let addr = if b.as_ptr() as usize % 8 == 0 {
            b.as_ptr() as usize
        } else {
            let mut v = vec![0u8; b.len() + 8].leak();
            let off = (8 - v.as_ptr() as usize % 8) % 8;
            v = &mut v[off..];
            v[..b.len()].copy_from_slice(b);
            v.as_ptr() as usize
        };
        unsafe { Dtb::from_addr(addr) }.unwrap()
    }

    #[test]
    fn the_tree_is_the_golden_bytes() {
        let b = small_tree();
        let h: std::string::String = b.iter().map(|x| std::format!("{x:02x}")).collect();
        assert_eq!(h, SMALL_TREE_HEX);
    }

    #[test]
    fn the_header_and_blocks_are_laid_out_like_dtc() {
        let b = small_tree();
        let be = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
        assert_eq!(be(0), 0xd00d_feed);
        assert_eq!(be(4), b.len());
        assert_eq!(be(16), 40, "reservation block right after the header");
        assert_eq!(be(20), 17);
        assert_eq!(be(24), 16);
        assert_eq!(be(8), 40 + 32, "one reservation and the terminator");
        assert_eq!(be(12), be(8) + be(36));
        assert_eq!(be(32), b.len() - be(12));
        assert_eq!(&b[40..48], &0x1000u64.to_be_bytes());
        assert_eq!(&b[48..56], &0x2000u64.to_be_bytes());
        assert!(b[56..72].iter().all(|&c| c == 0));
        // The strings are shared: "reg", "model"... each once.
        let strings = &b[be(12)..];
        assert_eq!(strings.split(|&c| c == 0).filter(|s| !s.is_empty()).count(), 8);
    }

    #[test]
    fn the_reader_reads_back_what_the_writer_wrote() {
        let blob = small_tree();
        let d = dtb(&blob);
        assert_eq!(d.total_size(), blob.len());
        let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
        assert_eq!(d.memory(&mut banks), 1);
        assert_eq!((banks[0].base, banks[0].size), (0x8000_0000, 0x4000_0000));
        assert_eq!(d.root_compatible(), b"a,b\0c,d\0");
        assert_eq!(d.find(b"", b"model").unwrap(), b"test\0");
        let mut r = [MemRange { base: 0, size: 0 }; 4];
        assert_eq!(d.mem_reserve(&mut r), 1);
        assert_eq!((r[0].base, r[0].size), (0x1000, 0x2000));
        let mut reg = [(0u64, 0u64); 2];
        assert_eq!(d.node_reg(b"memory", &mut reg), 1);
        assert_eq!(reg[0], (0x8000_0000, 0x4000_0000));
        let _ = (Conduit::Smc, EnableMethod::Psci);
    }


    /// The shape of the Pixel 2's tree where it matters: 64-bit cells at the root and in
    /// `/reserved-memory` (fixed children with a `reg`, dynamic ones with a `size`), 32-bit cells
    /// in `/soc` (the GIC with its distributor and redistributor region, the display).
    pub fn walleye_like() -> Vec<u8> {
        let mut out = vec![0u8; 8192];
        let mut strs = vec![0u8; 1024];
        let mut w = Writer::new(&mut out, &mut strs);
        w.begin_node("").unwrap();
        w.prop_u32("#address-cells", 2).unwrap();
        w.prop_u32("#size-cells", 2).unwrap();
        w.begin_node("memory").unwrap();
        w.prop_cells("reg", &[0, 0x8000_0000, 0, 0x7f00_0000, 1, 0, 0, 0x8000_0000]).unwrap();
        w.end_node().unwrap();
        w.begin_node("reserved-memory").unwrap();
        w.prop_u32("#address-cells", 2).unwrap();
        w.prop_u32("#size-cells", 2).unwrap();
        w.prop_empty("ranges").unwrap();
        for (name, base, size) in [
            ("removed_regions@85800000", 0x8580_0000u32, 0x370_0000u32),
            ("splash_region@9d600000", 0x9d60_0000, 0x240_0000),
        ] {
            w.begin_node(name).unwrap();
            w.prop_cells("reg", &[0, base, 0, size]).unwrap();
            w.end_node().unwrap();
        }
        w.begin_node("adsp_region").unwrap();
        w.prop_cells("size", &[0, 0x80_0000]).unwrap();
        w.end_node().unwrap();
        w.end_node().unwrap();
        w.begin_node("soc").unwrap();
        w.prop_u32("#address-cells", 1).unwrap();
        w.prop_u32("#size-cells", 1).unwrap();
        w.begin_node("interrupt-controller@17a00000").unwrap();
        w.prop_str("compatible", "arm,gic-v3").unwrap();
        w.prop_cells("reg", &[0x17a0_0000, 0x1_0000, 0x17b0_0000, 0x10_0000]).unwrap();
        w.end_node().unwrap();
        w.begin_node("timer").unwrap();
        w.prop_str("compatible", "arm,armv8-timer").unwrap();
        w.prop_cells("interrupts", &[1, 1, 0xf08, 1, 2, 0xf08, 1, 3, 0xf08, 1, 0, 0xf08]).unwrap();
        w.end_node().unwrap();
        w.begin_node("qcom,mdss_mdp@c900000").unwrap();
        w.prop_cells("reg", &[0xc90_0000, 0x9_0000, 0xc9b_0000, 0x1040]).unwrap();
        w.end_node().unwrap();
        w.end_node().unwrap();
        w.end_node().unwrap();
        let n = w.finish(&[(0xaff0_0000, 0x1000)]).unwrap();
        out.truncate(n);
        out
    }

    #[test]
    fn reserved_regions_and_soc_nodes_are_read_with_the_right_cell_sizes() {
        let blob = walleye_like();
        let d = dtb(&blob);
        let mut r = [MemRange { base: 0, size: 0 }; 8];
        let n = d.reserved_regions(&mut r);
        // The memreserve entry, then the two fixed children; the dynamic one has no address yet.
        assert_eq!(n, 3);
        assert_eq!((r[0].base, r[0].size), (0xaff0_0000, 0x1000));
        assert_eq!((r[1].base, r[1].size), (0x8580_0000, 0x370_0000));
        assert_eq!((r[2].base, r[2].size), (0x9d60_0000, 0x240_0000));
        let mut names = std::vec::Vec::new();
        d.each_reserved_child(|name, a, s| names.push((std::string::String::from_utf8_lossy(name).into_owned(), a, s)));
        assert_eq!(names[1], ("splash_region@9d600000".into(), 0x9d60_0000, 0x240_0000));
        let mut reg = [(0u64, 0u64); 4];
        // /soc has one cell for addresses and sizes: four numbers make two pairs.
        assert_eq!(d.node_reg(b"interrupt-controller@", &mut reg), 2);
        assert_eq!(reg[0], (0x17a0_0000, 0x1_0000));
        assert_eq!(reg[1], (0x17b0_0000, 0x10_0000));
        // By compatible, whatever the node is called.
        assert_eq!(d.node_reg_compat(b"arm,gic-v3", &mut reg), 2);
        assert_eq!(reg[1], (0x17b0_0000, 0x10_0000));
        assert_eq!(d.node_reg_compat(b"arm,gic", &mut reg), 0, "whole strings only");
        assert_eq!(d.node_reg_compat(b"no,such", &mut reg), 0);
        // A property of the node found by compatible: the timer's four interrupts, as the GIC driver reads them.
        let t = crate::msm::timer_intids(d.prop_compat(b"arm,armv8-timer", b"interrupts").unwrap());
        assert_eq!((t.phys, t.virt), (Some(18), Some(19)));
        assert_eq!(d.prop_compat(b"arm,armv8-timer", b"reg"), None);
        assert_eq!(d.prop_compat(b"arm,armv7-timer", b"interrupts"), None);
        assert_eq!(d.node_reg(b"qcom,mdss_mdp@", &mut reg), 2);
        assert_eq!(reg[0], (0xc90_0000, 0x9_0000));
        // The root's memory node: two banks, one above 4 GiB.
        let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
        assert_eq!(d.memory(&mut banks), 2);
        assert_eq!(banks[1].base, 0x1_0000_0000);
        assert_eq!(d.node_reg(b"nothing", &mut reg), 0);
        // Everything together: RAM minus the reserved regions (the splash stays in RAM for us: the
        // kernel takes it out of the list it hands to carve()).
        let mut left = [MemRange { base: 0, size: 0 }; 16];
        let k = crate::msm::carve(&banks[..2], &r[1..2], 4096, &mut left);
        assert_eq!(k, 3);
        assert_eq!(left[0].base + left[0].size, 0x8580_0000);
        assert_eq!(left[1].base, 0x8580_0000 + 0x370_0000);
    }

    #[test]
    fn hostile_trees_do_not_panic_the_new_readers() {
        let good = walleye_like();
        // Cut the blob at every length; truncated total_size so the reader's own checks bite.
        for cut in (0..good.len()).step_by(7) {
            let mut b = good[..cut].to_vec();
            if b.len() < 48 {
                continue;
            }
            let total = (b.len() as u32).to_be_bytes();
            b[4..8].copy_from_slice(&total);
            // Keep the declared block sizes inside the cut blob, as a damaged-but-plausible tree would.
            let st_off = u32::from_be_bytes([b[8], b[9], b[10], b[11]]) as usize;
            let st_size = u32::from_be_bytes([b[36], b[37], b[38], b[39]]) as usize;
            if st_off.saturating_add(st_size) > b.len() {
                continue;
            }
            let d = dtb(&b);
            let mut r = [MemRange { base: 0, size: 0 }; 8];
            let _ = d.reserved_regions(&mut r);
            let mut reg = [(0u64, 0u64); 4];
            let _ = d.node_reg(b"interrupt", &mut reg);
        }
        // Bad cell counts give nothing.
        assert_eq!(crate::dtb::parse_reg(&[0; 16], 3, 1, &mut [(0, 0); 2]), 0);
        assert_eq!(crate::dtb::parse_reg(&[0; 16], 0, 1, &mut [(0, 0); 2]), 0);
        assert_eq!(crate::dtb::parse_reg(&[0; 7], 1, 1, &mut [(0, 0); 2]), 0);
    }

    #[test]
    fn errors_are_reported_not_written_past() {
        let mut out = vec![0u8; 100];
        let mut strs = vec![0u8; 64];
        let mut w = Writer::new(&mut out, &mut strs);
        assert_eq!(w.prop_u32("x", 1), Err(Error::Structure));
        assert_eq!(w.end_node(), Err(Error::Structure));
        // The 100-byte buffer cannot even hold the reservation room.
        assert_eq!(w.begin_node(""), Err(Error::Full));
        let mut out = vec![0u8; 2048];
        let mut strs = vec![0u8; 8];
        let mut w = Writer::new(&mut out, &mut strs);
        w.begin_node("").unwrap();
        assert_eq!(w.prop_u32("a-long-name", 1), Err(Error::Full));
        assert_eq!(w.finish(&[]), Err(Error::Structure), "node still open");
        let mut out = vec![0u8; 2048];
        let mut strs = vec![0u8; 64];
        let mut w = Writer::new(&mut out, &mut strs);
        w.begin_node("").unwrap();
        w.end_node().unwrap();
        let too_many = [(1u64, 1u64); 70];
        assert_eq!(w.finish(&too_many), Err(Error::Full));
    }
}
