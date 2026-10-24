//! Host tests: a hand-written device tree (the shape of a Raspberry Pi 4's, small), command
//! lines, mailbox messages and SD answers, plus hostile versions of each.

use crate::cmdline;
use crate::dtb::{Conduit, Cpu, Dtb, EnableMethod, MAX_BANKS, MAX_CPUS, MemRange};
use crate::mailbox::{self, Builder, Error, Reply, tag};
use crate::sd::{self, CardState, Command, Dir, Resp};
use std::vec::Vec;

// ---- a tiny device tree writer ----------------------------------------------------------

/// Builds a flattened device tree blob the way dtc would.
struct Fdt {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Fdt {
    fn new() -> Fdt {
        Fdt {
            structure: Vec::new(),
            strings: Vec::new(),
        }
    }
    fn word(&mut self, v: u32) {
        self.structure.extend_from_slice(&v.to_be_bytes());
    }
    fn pad(&mut self) {
        while self.structure.len() % 4 != 0 {
            self.structure.push(0);
        }
    }
    fn begin(&mut self, name: &str) {
        self.word(1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }
    fn end(&mut self) {
        self.word(2);
    }
    fn string_off(&mut self, name: &str) -> u32 {
        let needle: Vec<u8> = name.bytes().chain([0]).collect();
        if let Some(p) = self
            .strings
            .windows(needle.len())
            .position(|w| w == &needle[..])
        {
            return p as u32;
        }
        let off = self.strings.len() as u32;
        self.strings.extend_from_slice(&needle);
        off
    }
    fn prop(&mut self, name: &str, data: &[u8]) {
        let off = self.string_off(name);
        self.word(3);
        self.word(data.len() as u32);
        self.word(off);
        self.structure.extend_from_slice(data);
        self.pad();
    }
    fn cells(&mut self, name: &str, v: &[u32]) {
        let d: Vec<u8> = v.iter().flat_map(|c| c.to_be_bytes()).collect();
        self.prop(name, &d);
    }
    fn str_prop(&mut self, name: &str, s: &str) {
        let mut d = s.as_bytes().to_vec();
        d.push(0);
        self.prop(name, &d);
    }
    /// The finished blob, 8-byte aligned in memory (the reader wants that).
    fn finish(mut self) -> Blob {
        self.word(9);
        let off_struct = 40 + 16; // header + one (empty) memory reservation entry
        let off_strings = off_struct + self.structure.len();
        let total = off_strings + self.strings.len();
        let mut b = Vec::new();
        for v in [
            0xd00d_feedu32,
            total as u32,
            off_struct as u32,
            off_strings as u32,
            40, // off_mem_rsvmap
            17, // version
            16, // last_comp_version
            0,  // boot_cpuid_phys
            self.strings.len() as u32,
            self.structure.len() as u32,
        ] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&self.structure);
        b.extend_from_slice(&self.strings);
        Blob::new(&b)
    }
}

/// Bytes at an 8-byte aligned address.
struct Blob {
    words: Vec<u64>,
    len: usize,
}

impl Blob {
    fn new(bytes: &[u8]) -> Blob {
        let mut words = std::vec![0u64; bytes.len().div_ceil(8) + 1];
        // SAFETY: the Vec<u64> is at least as long as `bytes` in bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr() as *mut u8, bytes.len())
        };
        Blob {
            words,
            len: bytes.len(),
        }
    }
    fn addr(&self) -> usize {
        self.words.as_ptr() as usize
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above.
        unsafe { core::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u8, self.len) }
    }
    fn dtb(&self) -> Option<Dtb> {
        // SAFETY: the blob lives as long as `self`; tests do not outlive it.
        unsafe { Dtb::from_addr(self.addr()) }
    }
}

const BOOTARGS: &str =
    "console=serial0,115200 root=/dev/mmcblk0p2 milk.selftest=full milk.net=\"internet=on dns=10.0.2.2:53\" quiet";

/// The shape of the stock Raspberry Pi 4 tree: 2-cell addresses, 1-cell sizes, a 4 GB memory
/// node (two banks), four spin-table cpus, no psci node.
fn pi_tree(psci: Option<&str>, spin: bool) -> Blob {
    let mut f = Fdt::new();
    f.begin("");
    f.prop("compatible", b"raspberrypi,4-model-b\0brcm,bcm2711\0");
    f.cells("#address-cells", &[2]);
    f.cells("#size-cells", &[1]);
    f.begin("chosen");
    f.str_prop("bootargs", BOOTARGS);
    f.end();
    f.begin("memory@0");
    f.str_prop("device_type", "memory");
    f.cells("reg", &[0, 0, 0x3b40_0000, 0, 0x4000_0000, 0xbc00_0000]);
    f.end();
    f.begin("cpus");
    f.cells("#address-cells", &[1]);
    f.cells("#size-cells", &[0]);
    for i in 0..4u32 {
        f.begin(&std::format!("cpu@{i}"));
        f.str_prop("device_type", "cpu");
        f.str_prop("compatible", "arm,cortex-a72");
        f.cells("reg", &[i]);
        if spin {
            f.str_prop("enable-method", "spin-table");
            f.cells("cpu-release-addr", &[0, 0xd8 + 8 * i]);
        } else {
            f.str_prop("enable-method", "psci");
        }
        f.end();
    }
    f.end();
    if let Some(m) = psci {
        f.begin("psci");
        f.str_prop("compatible", "arm,psci-1.0");
        f.str_prop("method", m);
        f.end();
    }
    f.end();
    f.finish()
}

#[test]
fn dtb_header_checks() {
    let b = pi_tree(None, true);
    let d = b.dtb().expect("valid tree");
    assert_eq!(d.total_size(), b.len);
    // Wrong magic, null, misaligned.
    let mut bad = pi_tree(None, true);
    bad.bytes_mut()[0] ^= 0xff;
    assert!(bad.dtb().is_none());
    assert!(unsafe { Dtb::from_addr(0) }.is_none());
    assert!(unsafe { Dtb::from_addr(b.addr() + 4) }.is_none());
}

#[test]
fn dtb_memory_banks() {
    let b = pi_tree(None, true);
    let d = b.dtb().unwrap();
    let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let n = d.memory(&mut banks);
    assert_eq!(n, 2);
    assert_eq!((banks[0].base, banks[0].size), (0, 0x3b40_0000));
    assert_eq!((banks[1].base, banks[1].size), (0x4000_0000, 0xbc00_0000));
}

#[test]
fn dtb_compatible_and_bootargs() {
    let b = pi_tree(None, true);
    let d = b.dtb().unwrap();
    assert_eq!(d.root_compatible(), b"raspberrypi,4-model-b\0brcm,bcm2711\0");
    assert_eq!(d.bootargs(), BOOTARGS.as_bytes());
    // No chosen node: empty.
    let mut f = Fdt::new();
    f.begin("");
    f.end();
    let empty = f.finish();
    assert_eq!(empty.dtb().unwrap().bootargs(), b"");
    assert_eq!(empty.dtb().unwrap().root_compatible(), b"");
}

#[test]
fn dtb_cpus_spin_table() {
    let b = pi_tree(None, true);
    let d = b.dtb().unwrap();
    let mut cpus = [Cpu {
        reg: 99,
        method: EnableMethod::Unknown,
        release_addr: 0,
    }; MAX_CPUS];
    assert_eq!(d.cpus(&mut cpus), 4);
    for i in 0..4 {
        assert_eq!(cpus[i].reg, i as u64);
        assert_eq!(cpus[i].method, EnableMethod::SpinTable);
        assert_eq!(cpus[i].release_addr, 0xd8 + 8 * i as u64);
    }
    assert_eq!(d.psci_conduit(), None);
}

#[test]
fn dtb_cpus_psci() {
    let b = pi_tree(Some("hvc"), false);
    let d = b.dtb().unwrap();
    let mut cpus = [Cpu {
        reg: 0,
        method: EnableMethod::Unknown,
        release_addr: 0,
    }; MAX_CPUS];
    assert_eq!(d.cpus(&mut cpus), 4);
    assert!(cpus[..4].iter().all(|c| c.method == EnableMethod::Psci));
    assert_eq!(d.psci_conduit(), Some(Conduit::Hvc));
    let b = pi_tree(Some("smc"), false);
    assert_eq!(b.dtb().unwrap().psci_conduit(), Some(Conduit::Smc));
}

#[test]
fn dtb_survives_damage() {
    // Flip every byte of the structure and strings in turn: nothing may panic or read outside
    // the blob. (The header stays valid: its sizes are what bound the walk.)
    let good = pi_tree(Some("hvc"), true);
    for i in 56..good.len {
        for flip in [0xffu8, 0x01, 0x80] {
            let mut b = Blob::new(&good.words_as_bytes());
            b.bytes_mut()[i] ^= flip;
            let Some(d) = b.dtb() else { continue };
            let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
            let _ = d.memory(&mut banks);
            let mut cpus = [Cpu {
                reg: 0,
                method: EnableMethod::Unknown,
                release_addr: 0,
            }; MAX_CPUS];
            let _ = d.cpus(&mut cpus);
            let _ = d.bootargs();
            let _ = d.root_compatible();
            let _ = d.psci_conduit();
        }
    }
}

#[test]
fn dtb_truncated_structure() {
    // A header that claims more structure than the blob has is refused; one that is cut
    // short inside a property gives what came before.
    let good = pi_tree(None, true);
    let mut b = Blob::new(&good.words_as_bytes());
    // struct size field (offset 36) larger than the blob.
    b.bytes_mut()[36..40].copy_from_slice(&(good.len as u32 * 2).to_be_bytes());
    assert!(b.dtb().is_none());
}

impl Blob {
    fn words_as_bytes(&self) -> Vec<u8> {
        // SAFETY: as in bytes_mut.
        unsafe { core::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.len) }.to_vec()
    }
}

// ---- command line ------------------------------------------------------------------------

#[test]
fn cmdline_finds_options() {
    let l = BOOTARGS.as_bytes();
    assert_eq!(cmdline::value(l, "selftest"), Some(&b"full"[..]));
    assert_eq!(
        cmdline::value(l, "net"),
        Some(&b"internet=on dns=10.0.2.2:53"[..])
    );
    assert_eq!(cmdline::value(l, "drivers"), None);
    // A prefix of another name is not it.
    assert_eq!(cmdline::value(l, "self"), None);
    assert_eq!(cmdline::value(l, "selftestx"), None);
    // Not ours: no `milk.` prefix.
    assert_eq!(cmdline::value(b"selftest=full", "selftest"), None);
    assert_eq!(cmdline::value(b"xmilk.selftest=full", "selftest"), None);
}

#[test]
fn cmdline_edge_cases() {
    assert_eq!(cmdline::value(b"", "x"), None);
    assert_eq!(cmdline::value(b"milk.x", "x"), None); // no '='
    assert_eq!(cmdline::value(b"milk.x=", "x"), Some(&b""[..]));
    assert_eq!(cmdline::value(b"milk.x=1 milk.x=2", "x"), Some(&b"1"[..]));
    assert_eq!(cmdline::value(b"milk.x=\"\"", "x"), Some(&b""[..]));
    assert_eq!(cmdline::value(b"milk.x=\"", "x"), Some(&b"\""[..])); // lone quote: kept
    assert_eq!(cmdline::value(b"a  milk.x=1\n", "x"), Some(&b"1"[..]));
    assert_eq!(cmdline::value(b"milk.x=1\0milk.y=2", "y"), Some(&b"2"[..]));
    assert_eq!(cmdline::value(b"milk.x=\"a b", "x"), Some(&b"\"a b"[..])); // unterminated
    assert_eq!(cmdline::find(b"milk.x=\"a b\"", "x"), Some((8, 3)));
    assert_eq!(cmdline::option_name("opt/milkos/selftest"), Some("selftest"));
    assert_eq!(cmdline::option_name("opt/milkos/"), None);
    assert_eq!(cmdline::option_name("etc/ramfb"), None);
}

// ---- mailbox -----------------------------------------------------------------------------

#[test]
fn mailbox_message_layout() {
    let mut buf = [0xdead_beefu32; 32];
    let mut b = Builder::new(&mut buf);
    let t = b.tag(tag::GET_CLOCK_RATE, &[clock_emmc2()], 2).unwrap();
    let n = b.finish().unwrap();
    // size, request, tag, buffer size, request/response, value words (2), end, padding
    assert_eq!(buf[0], 4 * 8);
    assert_eq!(buf[1], 0);
    assert_eq!(&buf[2..7], &[tag::GET_CLOCK_RATE, 8, 0, 12, 0]);
    assert_eq!(buf[7], 0); // end tag
    assert_eq!(n, 8);
    assert_eq!(n % 4, 0);
    // Not answered yet.
    assert_eq!(Reply::new(&buf).check(), Err(Error::NotAnswered));
    // The firmware answers.
    buf[1] = mailbox::RESPONSE_OK;
    buf[4] = 0x8000_0008;
    buf[5] = 12;
    buf[6] = 12_345_678;
    let r = Reply::new(&buf);
    assert_eq!(r.word(t, 1), Ok(12_345_678));
    assert_eq!(r.values(t).unwrap().len(), 2);
}

fn clock_emmc2() -> u32 {
    mailbox::clock::EMMC2
}

#[test]
fn mailbox_overflow_and_errors() {
    let mut small = [0u32; 6];
    let b = &mut Builder::new(&mut small);
    assert!(b.tag(tag::GET_PITCH, &[], 1).is_none()); // 3 + 1 words + end tag does not fit in 4 left
    let mut buf = [0u32; 16];
    let mut b = Builder::new(&mut buf);
    let t = b.tag(tag::GET_PITCH, &[], 1).unwrap();
    b.finish().unwrap();
    buf[1] = mailbox::RESPONSE_ERROR;
    assert_eq!(Reply::new(&buf).values(t), Err(Error::Failed(mailbox::RESPONSE_ERROR)));
    buf[1] = mailbox::RESPONSE_OK; // message ok but the tag was never processed
    assert_eq!(Reply::new(&buf).values(t), Err(Error::TagNotAnswered));
    // A tag handle that points past a shorter buffer.
    assert_eq!(Reply::new(&buf[..4]).values(t), Err(Error::Malformed));
    // The firmware claims more bytes than the value buffer: clamped.
    buf[4] = 0x8000_0000 | 400;
    assert_eq!(Reply::new(&buf).values(t).unwrap().len(), 1);
    // Missing response word.
    assert_eq!(Reply::new(&[]).check(), Err(Error::NotAnswered));
}

#[test]
fn mailbox_words() {
    assert_eq!(mailbox::mail_word(0x1000, 8), Some(0x1008));
    assert_eq!(mailbox::mail_word(0x1004, 8), None);
    assert_eq!(mailbox::mail_word(0x1000, 16), None);
    assert_eq!(mailbox::split_mail(0x1008), (0x1000, 8));
    assert_eq!(mailbox::bus_to_phys(0xc3a2_0000), 0x03a2_0000);
    assert_eq!(mailbox::bus_to_phys(0x3a20_0000), 0x3a20_0000);
}

/// What a well-behaved firmware writes into a framebuffer request.
fn fake_fb_answer(buf: &mut [u32], w: u32, h: u32, bus: u32, size: u32, pitch: u32, depth: u32) {
    buf[1] = mailbox::RESPONSE_OK;
    // The Tag handles are opaque: walk the message and answer each tag by its id.
    let mut i = 2;
    while i + 3 < buf.len() && buf[i] != 0 {
        let id = buf[i];
        let words = (buf[i + 1] / 4) as usize;
        let vals: &[u32] = match id {
            tag::SET_PHYSICAL_SIZE | tag::SET_VIRTUAL_SIZE => &[w, h],
            tag::SET_DEPTH => &[depth],
            tag::SET_PIXEL_ORDER => &[0],
            tag::SET_ALPHA_MODE => &[2],
            tag::SET_VIRTUAL_OFFSET => &[0, 0],
            tag::ALLOCATE_BUFFER => &[bus, size],
            tag::GET_PITCH => &[pitch],
            _ => &[],
        };
        for (k, v) in vals.iter().enumerate() {
            buf[i + 3 + k] = *v;
        }
        buf[i + 2] = 0x8000_0000 | (vals.len() as u32 * 4);
        i += 3 + words;
    }
}

#[test]
fn mailbox_framebuffer_roundtrip() {
    let mut buf = [0u32; 64];
    let (n, tags) = mailbox::framebuffer_request(&mut buf, 1024, 768, 32).unwrap();
    assert!(n <= buf.len());
    assert!((buf[0] as usize) <= n * 4 && n * 4 - (buf[0] as usize) < 16); // padded to 16 bytes
    fake_fb_answer(&mut buf, 1024, 768, 0xc3a2_0000, 1024 * 768 * 4, 4096, 32);
    let fb = mailbox::framebuffer_reply(&buf, &tags).unwrap();
    assert_eq!(
        fb,
        mailbox::Framebuffer {
            phys: 0x03a2_0000,
            size: 3_145_728,
            width: 1024,
            height: 768,
            depth: 32,
            pitch: 4096,
            order: 0
        }
    );
}

#[test]
fn mailbox_framebuffer_pitch_comes_from_the_answer() {
    // The firmware may pad rows: the pitch is not width * 4.
    let mut buf = [0u32; 64];
    let (_, tags) = mailbox::framebuffer_request(&mut buf, 1366, 768, 32).unwrap();
    fake_fb_answer(&mut buf, 1366, 768, 0xc100_0000, 1408 * 768 * 4, 1408 * 4, 32);
    let fb = mailbox::framebuffer_reply(&buf, &tags).unwrap();
    assert_eq!(fb.pitch, 5632);
    assert_eq!(fb.width, 1366);
}

#[test]
fn mailbox_framebuffer_rejects_nonsense() {
    let cases: [(u32, u32, u32, u32, u32, u32, u32); 7] = [
        // w, h, bus, size, pitch, depth, which
        (0, 768, 0xc100_0000, 3 << 20, 4096, 32, 0), // zero width
        (1024, 768, 0, 3 << 20, 4096, 32, 1),        // no address
        (1024, 768, 0xc100_0000, 3 << 20, 100, 32, 2), // pitch smaller than a row
        (1024, 768, 0xc100_0000, 1000, 4096, 32, 3), // buffer smaller than the screen
        (1024, 768, 0xc100_0000, 3 << 20, 4096, 16, 4), // not 32 bits per pixel
        (100_000, 768, 0xc100_0000, 3 << 20, 4096, 32, 5), // absurd width
        (1024, 768, 0xc100_0000, 3 << 20, 0, 32, 6), // pitch 0
    ];
    for (w, h, bus, size, pitch, depth, which) in cases {
        let mut buf = [0u32; 64];
        let (_, tags) = mailbox::framebuffer_request(&mut buf, 1024, 768, 32).unwrap();
        fake_fb_answer(&mut buf, w, h, bus, size, pitch, depth);
        assert!(
            mailbox::framebuffer_reply(&buf, &tags).is_err(),
            "case {which} accepted"
        );
    }
}

// ---- SD ----------------------------------------------------------------------------------

#[test]
fn sd_command_register_values() {
    // CMD0: no answer, no checks.
    let c = Command::new(sd::cmd::GO_IDLE_STATE, 0, Resp::None);
    assert_eq!(sd::sdhci_command(&c), 0);
    // CMD8: R7 (48 bits, CRC + index checked).
    let c = Command::new(sd::cmd::SEND_IF_COND, sd::IF_COND_ARG, Resp::R48);
    assert_eq!(sd::sdhci_command(&c), (8 << 8) | 0b10 | 1 << 3 | 1 << 4);
    // ACMD41: R3, neither check (its CRC field is all ones, the index field reserved).
    let c = Command::new(sd::cmd::SD_SEND_OP_COND, sd::op_cond_arg(true), Resp::R48);
    assert_eq!(sd::sdhci_command(&c), (41 << 8) | 0b10);
    // CMD2: R2, CRC check only.
    let c = Command::new(sd::cmd::ALL_SEND_CID, 0, Resp::R136);
    assert_eq!(sd::sdhci_command(&c), (2 << 8) | 0b01 | 1 << 3);
    // CMD7: R1b.
    let c = Command::new(sd::cmd::SELECT_CARD, 0x1234_0000, Resp::R48Busy);
    assert_eq!(sd::sdhci_command(&c), (7 << 8) | 0b11 | 1 << 3 | 1 << 4);
}

#[test]
fn sd_data_commands() {
    let mut read = Command::new(sd::cmd::READ_SINGLE_BLOCK, 0, Resp::R48);
    read.data = Some(Dir::Read);
    assert_eq!(
        sd::sdhci_command(&read),
        (17 << 8) | 0b10 | 1 << 3 | 1 << 4 | 1 << 5
    );
    assert_eq!(sd::sdhci_mode(&read, false), 1 << 4);
    assert_eq!(sd::sdhci_mode(&read, true), 1 << 4 | 1);
    let mut multi = Command::new(sd::cmd::WRITE_MULTIPLE_BLOCK, 0, Resp::R48);
    multi.data = Some(Dir::Write);
    multi.multi = true;
    multi.auto_stop = true;
    assert_eq!(sd::sdhci_mode(&multi, false), 1 << 5 | 1 << 1 | 1 << 2);
    // No data phase: no mode bits whatever else is set.
    let none = Command::new(sd::cmd::SEND_STATUS, 1 << 16, Resp::R48);
    assert_eq!(sd::sdhci_mode(&none, true), 0);
    // The combined word: command above, mode below.
    assert_eq!(
        sd::sdhci_cmd_word(&read, false),
        (sd::sdhci_command(&read) as u32) << 16 | (1 << 4)
    );
}

#[test]
fn sd_addressing() {
    assert_eq!(sd::data_arg(100, true), Some(100));
    assert_eq!(sd::data_arg(100, false), Some(51_200));
    assert_eq!(sd::data_arg(u64::from(u32::MAX) + 1, true), None);
    assert_eq!(sd::data_arg(1 << 23, false), None); // 4 GiB of bytes does not fit
    assert_eq!(sd::data_arg((1 << 23) - 1, false), Some(0xffff_fe00));
}

#[test]
fn sd_init_answers() {
    assert!(sd::if_cond_ok(0x0000_01aa));
    assert!(!sd::if_cond_ok(0x0000_0100));
    assert!(!sd::if_cond_ok(0));
    assert_eq!(sd::op_cond_arg(true), 0x40ff_8000);
    assert_eq!(sd::op_cond_arg(false), 0x00ff_8000);
    assert!(sd::ocr_ready(0xc0ff_8000));
    assert!(sd::ocr_high_capacity(0xc0ff_8000));
    assert!(!sd::ocr_ready(0x40ff_8000)); // still powering up
    assert!(!sd::ocr_high_capacity(0x80ff_8000));
    assert_eq!(sd::parse_r6(0xabcd_0500), (0xabcd, 0x0500));
}

#[test]
fn sd_card_state() {
    assert_eq!(sd::card_state(4 << 9), CardState::Tran);
    assert_eq!(sd::card_state(1 << 8), CardState::Idle); // ready for data, state idle
    assert_eq!(sd::card_state(7 << 9), CardState::Prg);
    assert_eq!(sd::card_state(15 << 9), CardState::Other(15));
    assert_ne!(sd::R1_ERROR_MASK & (1 << 31), 0); // out of range
    assert_ne!(sd::R1_ERROR_MASK & (1 << 23), 0); // CRC error
    assert_eq!(sd::R1_ERROR_MASK & (1 << 25), 0); // card is locked: a state, not a failure of this command
    assert_eq!(sd::R1_ERROR_MASK & (1 << 8), 0); // ready for data
}

/// Put a CSD together bit by bit (127..0), then shift it down 8 bits as an SDHCI's response
/// registers hold it.
fn csd_regs(fields: &[(u32, u32, u64)]) -> [u32; 4] {
    let mut csd: u128 = 0;
    for &(hi, lo, v) in fields {
        let width = hi - lo + 1;
        assert!(width == 64 || v < (1u64 << width));
        csd |= (v as u128) << lo;
    }
    let r = csd >> 8;
    [r as u32, (r >> 32) as u32, (r >> 64) as u32, (r >> 96) as u32]
}

#[test]
fn sd_csd_v2_capacity() {
    // A "32 GB" SDHC card: C_SIZE = 61 055 -> (61 056) * 1024 blocks = 31 940 608 KiB.
    let regs = csd_regs(&[(127, 126, 1), (83, 80, 9), (69, 48, 61_055)]);
    let c = sd::parse_csd(regs).unwrap();
    assert_eq!(c.version, 1);
    assert_eq!(c.blocks, 61_056 * 1024);
    assert_eq!(sd::blocks_to_bytes(c.blocks), 61_056 * 1024 * 512);
}

#[test]
fn sd_csd_v1_capacity() {
    // 512 MB standard capacity: READ_BL_LEN 9, C_SIZE 4095, C_SIZE_MULT 0b101 (mult 128).
    // bytes = (4095 + 1) * 2^(5 + 2) * 2^9 = 4096 * 128 * 512 = 256 MiB.
    let regs = csd_regs(&[(127, 126, 0), (83, 80, 9), (73, 62, 4095), (49, 47, 5)]);
    let c = sd::parse_csd(regs).unwrap();
    assert_eq!(c.version, 0);
    assert_eq!(c.blocks, 4096 * 128);
    // READ_BL_LEN 10 (1 KiB blocks) still counts in 512-byte blocks.
    let regs = csd_regs(&[(127, 126, 0), (83, 80, 10), (73, 62, 99), (49, 47, 0)]);
    assert_eq!(sd::parse_csd(regs).unwrap().blocks, 100 * 4 * 1024 / 512);
}

#[test]
fn sd_csd_rejects_junk() {
    assert_eq!(sd::parse_csd(csd_regs(&[(127, 126, 2)])), None); // unknown structure
    assert_eq!(sd::parse_csd([0; 4]), None); // v0 with C_SIZE 0, mult 0, len 0 -> 4 bytes: 0 blocks
    assert_eq!(sd::parse_csd(csd_regs(&[(127, 126, 0), (83, 80, 15)])), None); // block length 32 KiB
}

#[test]
fn sd_clock_dividers() {
    // EMMC2's base clock is 100 MHz on the Pi 4 (the firmware says so, mailbox clock 12).
    assert_eq!(sd::clock_divider(100_000_000, 400_000), Some((125, 400_000)));
    assert_eq!(sd::clock_divider(100_000_000, 25_000_000), Some((2, 25_000_000)));
    assert_eq!(sd::clock_divider(100_000_000, 50_000_000), Some((1, 50_000_000)));
    // Never above the target: 30 MHz from 100 MHz needs n = 2 (25 MHz), not 1 (50 MHz).
    assert_eq!(sd::clock_divider(100_000_000, 30_000_000), Some((2, 25_000_000)));
    // Base already below the target: undivided.
    assert_eq!(sd::clock_divider(25_000_000, 50_000_000), Some((0, 25_000_000)));
    // Unknown base clock or zero target: no answer.
    assert_eq!(sd::clock_divider(0, 400_000), None);
    assert_eq!(sd::clock_divider(100_000_000, 0), None);
    // A target so low the divider caps (1023): the result is above it, not a wrong divider.
    let (n, hz) = sd::clock_divider(100_000_000, 10_000).unwrap();
    assert_eq!(n, 1023);
    assert_eq!(hz, 100_000_000 / 2046);
    // Register bits: low byte in 15:8, top two bits in 7:6, internal clock enable.
    assert_eq!(sd::clock_control(125), (125 << 8) | 1);
    assert_eq!(sd::clock_control(1023), (0xff << 8) | (3 << 6) | 1);
    assert_eq!(sd::clock_control(0), 1);
}
