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
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                words.as_mut_ptr() as *mut u8,
                bytes.len(),
            )
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

const BOOTARGS: &str = "console=serial0,115200 root=/dev/mmcblk0p2 milk.selftest=full milk.net=\"internet=on dns=10.0.2.2:53\" quiet";

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
    assert_eq!(
        d.root_compatible(),
        b"raspberrypi,4-model-b\0brcm,bcm2711\0"
    );
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
    for (i, c) in cpus[..4].iter().enumerate() {
        assert_eq!(c.reg, i as u64);
        assert_eq!(c.method, EnableMethod::SpinTable);
        assert_eq!(c.release_addr, 0xd8 + 8 * i as u64);
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
    assert_eq!(
        cmdline::option_name("opt/milkos/selftest"),
        Some("selftest")
    );
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
    assert_eq!(
        Reply::new(&buf).values(t),
        Err(Error::Failed(mailbox::RESPONSE_ERROR))
    );
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
    fake_fb_answer(
        &mut buf,
        1366,
        768,
        0xc100_0000,
        1408 * 768 * 4,
        1408 * 4,
        32,
    );
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
    [
        r as u32,
        (r >> 32) as u32,
        (r >> 64) as u32,
        (r >> 96) as u32,
    ]
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
    assert_eq!(
        sd::parse_csd(csd_regs(&[(127, 126, 0), (83, 80, 15)])),
        None
    ); // block length 32 KiB
}

#[test]
fn sd_clock_dividers() {
    // EMMC2's base clock is 100 MHz on the Pi 4 (the firmware says so, mailbox clock 12).
    assert_eq!(
        sd::clock_divider(100_000_000, 400_000),
        Some((125, 400_000))
    );
    assert_eq!(
        sd::clock_divider(100_000_000, 25_000_000),
        Some((2, 25_000_000))
    );
    assert_eq!(
        sd::clock_divider(100_000_000, 50_000_000),
        Some((1, 50_000_000))
    );
    // Never above the target: 30 MHz from 100 MHz needs n = 2 (25 MHz), not 1 (50 MHz).
    assert_eq!(
        sd::clock_divider(100_000_000, 30_000_000),
        Some((2, 25_000_000))
    );
    // Base already below the target: undivided.
    assert_eq!(
        sd::clock_divider(25_000_000, 50_000_000),
        Some((0, 25_000_000))
    );
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

// ---- RAM banks ---------------------------------------------------------------------------

fn mr(base: usize, size: usize) -> MemRange {
    MemRange { base, size }
}

#[test]
fn banks_pi4_gb4() {
    // 4 GB Pi: 948 MiB low, the rest from 1 GiB to the peripherals; the kernel at 0x80000.
    let tree = [mr(0, 0x3b40_0000), mr(0x4000_0000, 0xbc00_0000)];
    let (b, n) = crate::dtb::usable_banks(&tree, 0x8_0000, 0xfc00_0000, 4096);
    assert_eq!(n, 2);
    assert_eq!((b[0].base, b[0].size), (0, 0x3b40_0000));
    assert_eq!((b[1].base, b[1].size), (0x4000_0000, 0xbc00_0000));
}

#[test]
fn banks_clip_at_a_limit() {
    // A board that cannot use RAM above 4 GiB: that bank goes, the one crossing the limit is cut.
    let tree = [
        mr(0, 0x3b40_0000),
        mr(0x4000_0000, 0xc000_0000),
        mr(0x1_0000_0000, 0x1_0000_0000),
    ];
    let (b, n) = crate::dtb::usable_banks(&tree, 0x8_0000, 0xfc00_0000, 4096);
    assert_eq!(n, 2);
    assert_eq!(b[1].base + b[1].size, 0xfc00_0000);
}

/// An 8 GB board's memory node: the size cell is 32 bits, so the 4 GiB above the peripherals come
/// as two entries; they merge into one bank.
fn pi8_tree() -> Blob {
    let mut f = Fdt::new();
    f.begin("");
    f.cells("#address-cells", &[2]);
    f.cells("#size-cells", &[1]);
    f.begin("memory@0");
    f.str_prop("device_type", "memory");
    f.cells(
        "reg",
        &[
            0,
            0,
            0x3b40_0000, // 948 MiB
            0,
            0x4000_0000,
            0xbc00_0000, // up to the peripherals
            1,
            0,
            0x8000_0000, // 4 GiB .. 6 GiB
            1,
            0x8000_0000,
            0x8000_0000, // 6 GiB .. 8 GiB
        ],
    );
    f.end();
    f.end();
    f.finish()
}

#[test]
fn banks_pi4_gb8_uses_memory_above_4gib() {
    let b = pi8_tree();
    let d = b.dtb().unwrap();
    let mut tree = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    assert_eq!(d.memory(&mut tree), 4);
    let (banks, n) = crate::dtb::usable_banks(&tree[..4], 0x8_0000, usize::MAX, 4096);
    assert_eq!(n, 3, "the two high entries touch and merge");
    assert_eq!((banks[0].base, banks[0].size), (0, 0x3b40_0000));
    assert_eq!((banks[1].base, banks[1].size), (0x4000_0000, 0xbc00_0000));
    assert_eq!(
        (banks[2].base, banks[2].size),
        (0x1_0000_0000, 0x1_0000_0000)
    );
    let total: usize = banks[..n].iter().map(|b| b.size).sum();
    assert_eq!(total, 0x3b40_0000 + 0xbc00_0000 + 0x1_0000_0000);
}

#[test]
fn banks_the_holes_are_what_is_between() {
    // The allocator spans [0, 8 GiB); what the kernel reserves is each gap between banks.
    let b = pi8_tree();
    let d = b.dtb().unwrap();
    let mut tree = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let n = d.memory(&mut tree);
    let (banks, n) = crate::dtb::usable_banks(&tree[..n], 0x8_0000, usize::MAX, 4096);
    let mut holes = std::vec::Vec::new();
    let mut prev_end = banks[0].base;
    for b in &banks[..n] {
        if b.base > prev_end {
            holes.push((prev_end, b.base));
        }
        prev_end = prev_end.max(b.base + b.size);
    }
    assert_eq!(
        holes,
        [(0x3b40_0000, 0x4000_0000), (0xfc00_0000, 0x1_0000_0000)]
    );
}

#[test]
fn banks_below_the_kernel_are_dropped_and_order_is_fixed() {
    let tree = [mr(0x8000_0000, 0x1000_0000), mr(0x4000_0000, 0x1000_0000)];
    let (b, n) = crate::dtb::usable_banks(&tree, 0x4008_0000, usize::MAX, 4096);
    assert_eq!(n, 2);
    assert_eq!(b[0].base, 0x4000_0000);
    // Kernel in the upper bank: the lower one goes.
    let (b, n) = crate::dtb::usable_banks(&tree, 0x8008_0000, usize::MAX, 4096);
    assert_eq!(n, 1);
    assert_eq!(b[0].base, 0x8000_0000);
}

#[test]
fn banks_hostile_input() {
    // Overlap and touching banks merge; nonsense (zero size, wrap-around, kernel in no bank)
    // gives nothing usable and never panics.
    let tree = [
        mr(0, 0x2000_0000),
        mr(0x1000_0000, 0x2000_0000),
        mr(0x3000_0000, 0x1000),
    ];
    let (b, n) = crate::dtb::usable_banks(&tree, 0x8_0000, usize::MAX, 4096);
    assert_eq!(n, 1);
    assert_eq!((b[0].base, b[0].size), (0, 0x3000_1000));
    assert_eq!(
        crate::dtb::usable_banks(&[mr(0, 0)], 0x8_0000, usize::MAX, 4096).1,
        0
    );
    assert_eq!(
        crate::dtb::usable_banks(&[mr(usize::MAX - 10, 100)], 0x8_0000, usize::MAX, 4096).1,
        0
    );
    assert_eq!(
        crate::dtb::usable_banks(&[mr(0x4000_0000, 0x1000_0000)], 0x8_0000, usize::MAX, 4096).1,
        0
    );
    assert_eq!(
        crate::dtb::usable_banks(&[], 0x8_0000, usize::MAX, 4096).1,
        0
    );
    // Unaligned edges round inwards.
    let (b, n) = crate::dtb::usable_banks(&[mr(0x100, 0x3000)], 0x1000, usize::MAX, 4096);
    assert_eq!(n, 1);
    assert_eq!((b[0].base, b[0].size), (0x1000, 0x2000));
}

// ---- USB ---------------------------------------------------------------------------------

mod usb_tests {
    use crate::usb::*;
    use std::vec::Vec;

    #[test]
    fn setup_stage_for_get_descriptor() {
        let t = setup_stage(req::get_descriptor(req::DESC_DEVICE, 0, 18), true);
        // 80 06 00 01 00 00 12 00 on the wire.
        assert_eq!(t.param, 0x0012_0000_0100_0680);
        assert_eq!(t.status, 8);
        // type 2 (setup), immediate data, transfer type 3 (IN data stage), cycle 1.
        assert_eq!(t.control, 2 << 10 | 1 << 6 | 3 << 16 | 1);
        assert_eq!(t.kind(), trb_type::SETUP_STAGE);
        // No data stage: transfer type 0.
        let t = setup_stage(req::set_configuration(1), false);
        assert_eq!(t.control, 2 << 10 | 1 << 6);
        assert_eq!(t.param, 0x0000_0000_0001_0900);
        // OUT data: 2.
        let s = Setup {
            request_type: 0x21,
            request: 9,
            value: 0,
            index: 0,
            length: 1,
        };
        assert_eq!(setup_stage(s, false).control >> 16 & 3, 2);
    }

    #[test]
    fn data_status_normal_and_link_trbs() {
        let d = data_stage(0x1234_5000, 18, true, true);
        assert_eq!((d.param, d.status), (0x1234_5000, 18));
        assert_eq!(d.control, 3 << 10 | 1 << 16 | 1);
        let s = status_stage(false, true);
        assert_eq!(s.control, 4 << 10 | 1 << 5 | 1); // IOC, direction OUT
        let s = status_stage(true, false);
        assert_eq!(s.control, 4 << 10 | 1 << 5 | 1 << 16);
        let n = normal(0x8000, 8, false);
        assert_eq!(n.control, 1 << 10 | 1 << 5);
        let l = link(0x1000, true, true);
        assert_eq!(l.control, 6 << 10 | 1 << 1 | 1);
        assert_eq!(l.param, 0x1000);
        assert!(chained(n).control & 1 << 4 != 0);
        // Words in memory order and back.
        let w = d.words();
        assert_eq!(w, [0x1234_5000, 0, 18, 3 << 10 | 1 << 16 | 1]);
        assert_eq!(Trb::from_words(w), d);
        assert!(!d.with_cycle(false).cycle());
    }

    #[test]
    fn command_trbs() {
        assert_eq!(enable_slot(true).control, 9 << 10 | 1);
        let a = address_device(0x2000, 3, false, true);
        assert_eq!(a.param, 0x2000);
        assert_eq!(a.control, 11 << 10 | 3 << 24 | 1);
        assert_eq!(
            address_device(0x2000, 3, true, false).control,
            11 << 10 | 3 << 24 | 1 << 9
        );
        assert_eq!(
            configure_endpoint(0x3000, 2, true).control,
            12 << 10 | 2 << 24 | 1
        );
        assert_eq!(disable_slot(5, false).control, 10 << 10 | 5 << 24);
        assert_eq!(noop_command(true).kind(), trb_type::NO_OP_COMMAND);
    }

    #[test]
    fn events_decode() {
        // Command completion: slot 4 assigned, success.
        let t = Trb {
            param: 0x1010,
            status: (code::SUCCESS as u32) << 24,
            control: 33 << 10 | 4 << 24 | 1,
        };
        assert_eq!(
            decode_event(&t),
            Event::CommandCompletion {
                trb: 0x1010,
                code: 1,
                slot: 4
            }
        );
        // Transfer event: short packet, 3 bytes left, slot 2, endpoint DCI 3.
        let t = Trb {
            param: 0x5000,
            status: (code::SHORT_PACKET as u32) << 24 | 3,
            control: 32 << 10 | 3 << 16 | 2 << 24 | 1,
        };
        assert_eq!(
            decode_event(&t),
            Event::Transfer {
                trb: 0x5000,
                code: 13,
                residual: 3,
                slot: 2,
                dci: 3
            }
        );
        // Port status change: port 2.
        let t = Trb {
            param: 2 << 24,
            status: 1 << 24,
            control: 34 << 10 | 1,
        };
        assert_eq!(decode_event(&t), Event::PortStatusChange { port: 2 });
        assert_eq!(
            decode_event(&Trb {
                control: 37 << 10,
                ..Trb::default()
            }),
            Event::Other(37)
        );
    }

    #[test]
    fn port_register_writes_do_not_clear_by_accident() {
        // Connected, enabled, powered, with a connect-change and a reset-change pending, full speed.
        let v = portsc::CCS | portsc::PED | portsc::PP | portsc::CSC | portsc::PRC | 1 << 10;
        assert_eq!(portsc::speed(v), speed::FULL);
        // Clearing only the connect change: PED must not be written as 1 (that disables the port), the
        // other change bit must not be either.
        let w = portsc::write_clearing(v, portsc::CSC);
        assert_eq!(w & portsc::PED, 0);
        assert_eq!(w & portsc::PRC, 0);
        assert_ne!(w & portsc::CSC, 0);
        assert_ne!(w & portsc::PP, 0, "power stays on");
        // A reset: PR set, no change bits, no PED.
        let w = portsc::write_reset(v);
        assert_eq!(w & portsc::CHANGE_BITS, 0);
        assert_ne!(w & portsc::PR, 0);
        assert_eq!(w & portsc::PED, 0);
        // clear can only name change bits.
        assert_eq!(
            portsc::write_clearing(0, portsc::PR | portsc::CSC),
            portsc::CSC
        );
        assert_eq!(portsc::link_state(0x5 << 5), 5);
    }

    #[test]
    fn intervals_and_packet_sizes() {
        // Low speed keyboard asking 10 ms: 80 units of 125 us, rounded down to 64 = 2^6 (8 ms).
        assert_eq!(interrupt_interval(speed::LOW, 10), 6);
        assert_eq!(interrupt_interval(speed::FULL, 1), 3);
        assert_eq!(interrupt_interval(speed::FULL, 8), 6);
        assert_eq!(interrupt_interval(speed::FULL, 255), 10); // capped at 2^10 * 125 us
        assert_eq!(interrupt_interval(speed::FULL, 0), 3); // nonsense still gives a legal value
        // High speed counts 2^(n-1) microframes.
        assert_eq!(interrupt_interval(speed::HIGH, 4), 3);
        assert_eq!(interrupt_interval(speed::SUPER, 1), 0);
        assert_eq!(interrupt_interval(speed::HIGH, 0), 0);
        assert_eq!(interrupt_interval(speed::HIGH, 200), 15);
        assert_eq!(default_max_packet0(speed::LOW), 8);
        assert_eq!(default_max_packet0(speed::FULL), 64);
        assert_eq!(default_max_packet0(speed::SUPER), 512);
        assert_eq!(default_max_packet0(0), 8);
    }

    #[test]
    fn device_context_indexes() {
        assert_eq!(dci(0x00), 1);
        assert_eq!(dci(0x80), 1);
        assert_eq!(dci(0x81), 3);
        assert_eq!(dci(0x01), 2);
        assert_eq!(dci(0x83), 7);
        assert_eq!(dci(0x0f), 30);
        assert_eq!(dci(0x8f), 31);
    }

    #[test]
    fn contexts_are_laid_out_as_the_spec_says() {
        for csz in [false, true] {
            let n = ctx_dwords(csz);
            assert_eq!(input_slot_offset(csz), n);
            assert_eq!(input_ep_offset(csz, 1), 2 * n);
            assert_eq!(input_ep_offset(csz, 3), 4 * n);
            assert_eq!(input_ctx_dwords(csz), 33 * n);
            assert_eq!(device_ctx_dwords(csz), 32 * n);
        }
        let mut ctx = std::vec![0u32; input_ctx_dwords(false)];
        fill_input_control(&mut ctx, 0, 0b11);
        assert_eq!((ctx[0], ctx[1]), (0, 3));
        let so = input_slot_offset(false);
        fill_slot(&mut ctx[so..], speed::LOW, 1, 2);
        // speed 2 in 23:20, 1 context entry in 31:27, root port 2 in 23:16 of dword 1.
        assert_eq!(ctx[so], 2 << 20 | 1 << 27);
        assert_eq!(ctx[so + 1], 2 << 16);
        let eo = input_ep_offset(false, 1);
        fill_ep0(&mut ctx[eo..], 8, 0x1_2345_6000);
        // CErr 3, type 4 (control), max packet 8; dequeue pointer | DCS.
        assert_eq!(ctx[eo + 1], 3 << 1 | 4 << 3 | 8 << 16);
        assert_eq!(ctx[eo + 2], 0x2345_6001);
        assert_eq!(ctx[eo + 3], 1);
        let eo = input_ep_offset(false, 3);
        fill_interrupt_in(&mut ctx[eo..], 8, 6, 0x4000);
        assert_eq!(ctx[eo], 6 << 16);
        assert_eq!(ctx[eo + 1], 3 << 1 | 7 << 3 | 8 << 16);
        assert_eq!(ctx[eo + 2], 0x4001);
        assert_eq!(ctx[eo + 4], 8 | 8 << 16);
    }

    /// A configuration descriptor of a boot keyboard with a boot mouse interface (the shape of a
    /// combined receiver), then a vendor interface, then a keyboard alternate setting.
    fn composite_config() -> Vec<u8> {
        let mut c = std::vec![
            9, 2, 0, 0, 3, 1, 0, 0xa0, 50, // configuration: 3 interfaces, value 1
            // interface 0: HID boot keyboard
            9, 4, 0, 0, 1, 3, 1, 1, 0, 9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63,
            0, // HID descriptor
            7, 5, 0x81, 3, 8, 0, 10, // endpoint IN 1, interrupt, 8 bytes, 10 ms
            // interface 1: HID boot mouse
            9, 4, 1, 0, 1, 3, 1, 2, 0, 9, 0x21, 0x11, 0x01, 0, 1, 0x22, 52, 0, 7, 5, 0x82, 3, 4, 0,
            8, // interface 2: vendor specific with bulk endpoints
            9, 4, 2, 0, 2, 0xff, 0, 0, 0, 7, 5, 0x03, 2, 64, 0, 0, 7, 5, 0x84, 2, 64, 0, 0,
        ];
        let total = c.len() as u16;
        c[2..4].copy_from_slice(&total.to_le_bytes());
        c
    }

    #[test]
    fn finds_the_hid_boot_interfaces() {
        let cfg = composite_config();
        assert_eq!(config_total_length(&cfg), Some(cfg.len() as u16));
        let mut out = [HidBoot {
            config: 0,
            interface: 0,
            kind: HidKind::Keyboard,
            ep_addr: 0,
            max_packet: 0,
            interval: 0,
        }; MAX_HID];
        let n = find_hid_boot(&cfg, &mut out);
        assert_eq!(n, 2);
        assert_eq!(
            out[0],
            HidBoot {
                config: 1,
                interface: 0,
                kind: HidKind::Keyboard,
                ep_addr: 0x81,
                max_packet: 8,
                interval: 10
            }
        );
        assert_eq!(
            out[1],
            HidBoot {
                config: 1,
                interface: 1,
                kind: HidKind::Mouse,
                ep_addr: 0x82,
                max_packet: 4,
                interval: 8
            }
        );
    }

    #[test]
    fn descriptor_walk_survives_damage() {
        let good = composite_config();
        let mut out = [HidBoot {
            config: 0,
            interface: 0,
            kind: HidKind::Keyboard,
            ep_addr: 0,
            max_packet: 0,
            interval: 0,
        }; MAX_HID];
        // Every truncation and every single-byte corruption: no panic, never more than MAX_HID.
        for cut in 0..=good.len() {
            let n = find_hid_boot(&good[..cut], &mut out);
            assert!(n <= MAX_HID);
        }
        for i in 0..good.len() {
            for v in [0u8, 1, 0x7f, 0xff] {
                let mut bad = good.clone();
                bad[i] = v;
                assert!(find_hid_boot(&bad, &mut out) <= MAX_HID);
            }
        }
        // A zero-length descriptor must not loop forever.
        let mut z = good.clone();
        z[9] = 0;
        let _ = find_hid_boot(&z, &mut out);
        // A mouse-class interface with no endpoint yields nothing.
        let mut c = std::vec![9, 2, 18, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 0, 3, 1, 2, 0];
        let l = c.len() as u16;
        c[2..4].copy_from_slice(&l.to_le_bytes());
        assert_eq!(find_hid_boot(&c, &mut out), 0);
    }

    #[test]
    fn device_descriptor() {
        let d = [
            18u8, 1, 0, 2, 0, 0, 0, 8, 0x6d, 0x04, 0x16, 0xc0, 0, 0x20, 1, 2, 0, 1,
        ];
        let p = parse_device(&d).unwrap();
        assert_eq!(
            (p.class, p.max_packet0, p.vendor, p.product, p.num_configs),
            (0, 8, 0x046d, 0xc016, 1)
        );
        // Only the first 8 bytes: the packet size is there, the rest not.
        let p = parse_device(&d[..8]).unwrap();
        assert_eq!((p.max_packet0, p.vendor), (8, 0));
        assert!(parse_device(&d[..7]).is_none());
        assert!(
            parse_device(&[18, 2, 0, 0, 0, 0, 0, 8]).is_none(),
            "not a device descriptor"
        );
        assert_eq!(config_total_length(&[9, 2, 0x22, 0, 1]), Some(0x22));
        assert_eq!(config_total_length(&[9, 1, 0x22, 0]), None);
    }

    fn collect_keys(kb: &mut Keyboard, report: [u8; 8], now: u64) -> Vec<(u16, i32)> {
        let mut v = Vec::new();
        kb.report(&report, now, |c| v.push((c.evdev, c.value)));
        v
    }

    #[test]
    fn keyboard_press_release_and_modifiers() {
        let mut kb = Keyboard::new();
        // Press a.
        assert_eq!(
            collect_keys(&mut kb, [0, 0, 0x04, 0, 0, 0, 0, 0], 0),
            [(30, 1)]
        );
        // Left shift goes down while a is held, then z: shift first, then the new key.
        assert_eq!(
            collect_keys(&mut kb, [2, 0, 0x04, 0x1d, 0, 0, 0, 0], 10),
            [(42, 1), (44, 1)]
        );
        // Release a and shift together with z still down: letters first, modifiers last.
        assert_eq!(
            collect_keys(&mut kb, [0, 0, 0x1d, 0, 0, 0, 0, 0], 20),
            [(30, 0), (42, 0)]
        );
        assert_eq!(collect_keys(&mut kb, [0; 8], 30), [(44, 0)]);
        // A right-hand modifier.
        assert_eq!(
            collect_keys(&mut kb, [0x20, 0, 0, 0, 0, 0, 0, 0], 40),
            [(54, 1)]
        );
        assert_eq!(collect_keys(&mut kb, [0; 8], 50), [(54, 0)]);
        // Same report twice: nothing.
        assert_eq!(
            collect_keys(&mut kb, [0, 0, 0x28, 0, 0, 0, 0, 0], 60),
            [(28, 1)]
        );
        assert_eq!(collect_keys(&mut kb, [0, 0, 0x28, 0, 0, 0, 0, 0], 70), []);
    }

    #[test]
    fn keyboard_rollover_and_short_reports() {
        let mut kb = Keyboard::new();
        collect_keys(&mut kb, [0, 0, 0x04, 0, 0, 0, 0, 0], 0);
        // The phantom report (too many keys): ignored, a is still down.
        assert_eq!(collect_keys(&mut kb, [0, 0, 1, 1, 1, 1, 1, 1], 5), []);
        assert_eq!(collect_keys(&mut kb, [0; 8], 10), [(30, 0)]);
        let mut v = Vec::new();
        kb.report(&[0, 0, 0x04], 20, |c| v.push(c));
        assert!(v.is_empty(), "a report shorter than 8 bytes is ignored");
        // Unknown usages are skipped without disturbing the rest.
        assert_eq!(
            collect_keys(&mut kb, [0, 0, 0xe8, 0x04, 0, 0, 0, 0], 30),
            [(30, 1)]
        );
    }

    #[test]
    fn keyboard_repeats_the_last_key_after_a_delay() {
        let mut kb = Keyboard::new();
        collect_keys(&mut kb, [0, 0, 0x04, 0, 0, 0, 0, 0], 1000);
        let mut v = Vec::new();
        kb.tick(1400, |c| v.push(c));
        assert!(v.is_empty(), "not before the delay");
        kb.tick(1500, |c| v.push(c));
        assert_eq!(
            v,
            [KeyChange {
                evdev: 30,
                value: 2
            }]
        );
        v.clear();
        kb.tick(1510, |c| v.push(c));
        assert!(v.is_empty());
        kb.tick(1533, |c| v.push(c));
        assert_eq!(v.len(), 1);
        // A second key takes over the repeat; releasing it stops the repeat.
        collect_keys(&mut kb, [0, 0, 0x04, 0x05, 0, 0, 0, 0], 1600);
        v.clear();
        kb.tick(2200, |c| v.push(c));
        assert_eq!(
            v,
            [KeyChange {
                evdev: 48,
                value: 2
            }]
        );
        collect_keys(&mut kb, [0, 0, 0x04, 0, 0, 0, 0, 0], 2300);
        v.clear();
        kb.tick(3000, |c| v.push(c));
        assert!(
            v.is_empty(),
            "the repeating key was released; the older one does not take over"
        );
    }

    #[test]
    fn keyboard_release_all_on_unplug() {
        let mut kb = Keyboard::new();
        collect_keys(&mut kb, [0x01, 0, 0x04, 0x05, 0, 0, 0, 0], 0);
        let mut v = Vec::new();
        kb.release_all(|c| v.push((c.evdev, c.value)));
        assert_eq!(v, [(30, 0), (48, 0), (29, 0)]);
        let mut v = Vec::new();
        kb.release_all(|c| v.push(c));
        assert!(v.is_empty());
    }

    #[test]
    fn every_usage_maps_to_a_distinct_key() {
        let mut seen = std::collections::BTreeSet::new();
        for u in 0x04u8..=0x67 {
            let e = usage_to_evdev(u);
            if e != 0 {
                // (0x31 and 0x32 both mean the backslash key.)
                if u != 0x32 {
                    assert!(seen.insert(e), "usage {u:#x} repeats evdev code {e}");
                }
            }
        }
        assert_eq!(usage_to_evdev(0x04), 30); // a
        assert_eq!(usage_to_evdev(0x1d), 44); // z
        assert_eq!(usage_to_evdev(0x1e), 2); // 1
        assert_eq!(usage_to_evdev(0x27), 11); // 0
        assert_eq!(usage_to_evdev(0x3a), 59); // F1
        assert_eq!(usage_to_evdev(0x45), 88); // F12
        assert_eq!(usage_to_evdev(0x52), 103); // up
        assert_eq!(usage_to_evdev(0xff), 0);
        assert_eq!(usage_to_evdev(0x01), 0);
    }

    #[test]
    fn mouse_reports() {
        let m = parse_mouse(&[0b101, 5, 0xfb, 0xff]).unwrap();
        assert_eq!(
            m,
            MouseReport {
                buttons: 5,
                dx: 5,
                dy: -5,
                wheel: -1
            }
        );
        let m = parse_mouse(&[0, 127, 0x80]).unwrap();
        assert_eq!((m.dx, m.dy, m.wheel), (127, -128, 0));
        assert!(parse_mouse(&[1, 2]).is_none());
        assert_eq!(
            parse_mouse(&[0xff, 0, 0]).unwrap().buttons,
            7,
            "only the three buttons"
        );
    }
}

mod pci_tests {
    use crate::pci::*;

    #[test]
    fn ids_and_classes() {
        assert!(present(0x1234_8086));
        assert!(!present(0xffff_ffff));
        assert!(!present(0xaaaa_ffff));
        assert!(!present(0));
        assert_eq!((vendor(0x0d10_1106), device(0x0d10_1106)), (0x1106, 0x0d10));
        // The VL805: class 0c03, programming interface 30 (xHCI).
        assert_eq!(class_codes(0x0c03_3001), (0x0c, 0x03, 0x30));
        assert!(is_xhci(0x0c03_3001));
        assert!(!is_xhci(0x0c03_2001)); // EHCI
        assert!(!is_xhci(0x0604_0000)); // a bridge
        assert_eq!(header_type(0x0001_0000), (1, false));
        assert_eq!(header_type(0x0081_0000), (1, true));
        assert_eq!(header_type(0x0080_0000), (0, true));
    }

    #[test]
    fn bar_sizing() {
        // 64 KiB 32-bit BAR: all ones written, 0xffff0000 read back.
        assert_eq!(bar_kind(0xffff_0000), BarKind::Mem32 { prefetch: false });
        assert_eq!(bar_size(0xffff_0000, 0), 0x1_0000);
        // 16 KiB 64-bit non-prefetchable (qemu's xHCI): low dword ffffc004, high ffffffff.
        assert_eq!(bar_kind(0xffff_c004), BarKind::Mem64 { prefetch: false });
        assert_eq!(bar_size(0xffff_c004, 0xffff_ffff), 0x4000);
        // A 64-bit BAR of 8 GiB: the high dword has the size bits.
        assert_eq!(bar_size(0x0000_0004, 0xffff_fffe), 0x2_0000_0000);
        assert_eq!(bar_kind(0xffff_fff1), BarKind::Io);
        assert_eq!(bar_size(0xffff_fff1, 0), 0);
        assert_eq!(bar_kind(0), BarKind::None);
        assert_eq!(bar_size(0, 0), 0);
        assert_eq!(bar_kind(0xffff_fff8), BarKind::Mem32 { prefetch: true });
    }

    #[test]
    fn window_allocation_is_aligned_and_bounded() {
        let mut w = Window::new(0x1000_0000, 0x10_0000);
        assert_eq!(w.alloc(0x4000, 0x4000), Some(0x1000_0000));
        assert_eq!(w.alloc(0x1000, 0x1000), Some(0x1000_4000));
        // A big one aligns up, leaving a gap.
        assert_eq!(w.alloc(0x8_0000, 0x8_0000), Some(0x1008_0000));
        // The window is nearly gone.
        assert_eq!(w.alloc(0x8_0000, 0x8_0000), None);
        assert_eq!(w.alloc(0, 0x1000), None);
        assert_eq!(w.alloc(0x1000, 3), None, "alignment must be a power of two");
        assert!(w.alloc(u64::MAX, 1).is_none());
        // A window at the top of the address space does not wrap.
        let mut w = Window::new(u64::MAX - 0xfff, 0x1000);
        assert!(w.alloc(0x2000, 0x1000).is_none());
    }

    #[test]
    fn bridge_windows() {
        // 0x10000000 .. 0x10300000: base field 0x1000, limit field 0x102 (the last MiB).
        assert_eq!(
            bridge_mem_window(0x1000_0000, 0x1030_0000),
            Some(0x1020_1000)
        );
        assert_eq!(
            bridge_mem_window(0x1000_0000, 0x1010_0000),
            Some(0x1000_1000)
        );
        assert_eq!(bridge_mem_window(0x1000_0000, 0x1000_0000), None);
        assert_eq!(
            bridge_mem_window(0x1000_1000, 0x1020_0000),
            None,
            "not 1 MiB aligned"
        );
        assert_eq!(bridge_mem_window(0xff00_0000, 1 << 32), Some(0xfff0_ff00));
        assert_eq!(bridge_mem_window(0xff00_0000, (1 << 32) + (1 << 20)), None);
        // Closed: base 0xfff0 above limit 0x0000.
        const { assert!(BRIDGE_WINDOW_CLOSED & 0xfff0 > BRIDGE_WINDOW_CLOSED >> 16) };
    }

    #[test]
    fn host_bridge_ranges_from_a_virt_tree() {
        // qemu virt's pcie node: I/O at 0x3eff0000 (64 KiB), 32-bit memory 0x10000000 + 0x2eff0000,
        // 64-bit memory at 0x80_0000_0000. 7 cells each: 3 pci, 2 cpu, 2 size.
        let cells: [u32; 21] = [
            0x0100_0000,
            0,
            0,
            0,
            0x3eff_0000,
            0,
            0x1_0000,
            0x0200_0000,
            0,
            0x1000_0000,
            0,
            0x1000_0000,
            0,
            0x2eff_0000,
            0x0300_0000,
            0x80,
            0,
            0x80,
            0,
            0x80,
            0,
        ];
        let data: std::vec::Vec<u8> = cells.iter().flat_map(|c| c.to_be_bytes()).collect();
        let mut r = [Range {
            space: 0,
            prefetch: false,
            pci_addr: 0,
            cpu_addr: 0,
            size: 0,
        }; MAX_RANGES];
        assert_eq!(parse_ranges(&data, 2, 2, &mut r), 3);
        assert_eq!(
            r[0],
            Range {
                space: 1,
                prefetch: false,
                pci_addr: 0,
                cpu_addr: 0x3eff_0000,
                size: 0x1_0000
            }
        );
        assert_eq!(
            r[1],
            Range {
                space: 2,
                prefetch: false,
                pci_addr: 0x1000_0000,
                cpu_addr: 0x1000_0000,
                size: 0x2eff_0000
            }
        );
        assert_eq!(r[2].space, 3);
        assert_eq!(r[2].cpu_addr, 0x80_0000_0000);
        assert_eq!(memory_window(&r[..3]).unwrap().pci_addr, 0x1000_0000);
        // Truncated data gives only whole entries; nonsense cell counts give nothing.
        assert_eq!(parse_ranges(&data[..data.len() - 1], 2, 2, &mut r), 2);
        assert_eq!(parse_ranges(&data, 3, 2, &mut r), 0);
        assert_eq!(parse_ranges(&[], 2, 2, &mut r), 0);
    }

    #[test]
    fn host_bridge_ranges_from_a_pi_tree() {
        // The Pi 4's pcie@7d500000: one 32-bit memory range, PCI 0xf8000000 -> CPU 0x6_00000000, 64 MiB;
        // 2 parent cells, 1 size cell... (the tree has #size-cells = 2 in /scb, 7 cells: 3 + 2 + 2).
        let cells: [u32; 7] = [0x0200_0000, 0, 0xf800_0000, 6, 0, 0, 0x0400_0000];
        let data: std::vec::Vec<u8> = cells.iter().flat_map(|c| c.to_be_bytes()).collect();
        let mut r = [Range {
            space: 0,
            prefetch: false,
            pci_addr: 0,
            cpu_addr: 0,
            size: 0,
        }; MAX_RANGES];
        assert_eq!(parse_ranges(&data, 2, 2, &mut r), 1);
        assert_eq!(r[0].cpu_addr, 0x6_0000_0000);
        assert_eq!(r[0].pci_addr, 0xf800_0000);
        assert_eq!(r[0].size, 0x400_0000);
        let w = memory_window(&r[..1]).unwrap();
        assert_eq!(w.size, 64 << 20);
        assert!(memory_window(&[]).is_none());
    }
}

/// Parse a device tree the firmware ships (not in the repository: the path is in `MILK_REAL_DTB`, e.g.
/// bcm2711-rpi-4-b.dtb from a Pi's boot partition). Skipped when the variable is not set.
#[test]
fn a_real_pi4_tree_if_there_is_one() {
    let Some(path) = std::env::var_os("MILK_REAL_DTB") else {
        return;
    };
    let bytes = std::fs::read(path).expect("read MILK_REAL_DTB");
    let blob = Blob::new(&bytes);
    let d = blob.dtb().expect("a valid tree");
    assert!(crate::dtb::compat_has(d.root_compatible(), "brcm,bcm2711"));
    let mut cpus = [Cpu {
        reg: 0,
        method: EnableMethod::Unknown,
        release_addr: 0,
    }; MAX_CPUS];
    let n = d.cpus(&mut cpus);
    assert_eq!(n, 4);
    for (i, c) in cpus[..4].iter().enumerate() {
        assert_eq!(c.reg, i as u64);
        assert_eq!(c.method, EnableMethod::SpinTable);
        assert_eq!(c.release_addr, 0xd8 + 8 * i as u64);
    }
    assert_eq!(d.psci_conduit(), None);
    let reg = d.find_prefix(b"pcie@", b"reg").expect("a pcie node");
    assert_eq!(&reg[..8], &[0, 0, 0, 0, 0x7d, 0x50, 0x00, 0x00]);
    let ranges = d.find_prefix(b"pcie@", b"ranges").expect("its ranges");
    let mut r = [crate::pci::Range {
        space: 0,
        prefetch: false,
        pci_addr: 0,
        cpu_addr: 0,
        size: 0,
    }; crate::pci::MAX_RANGES];
    let k = crate::pci::parse_ranges(ranges, 2, 2, &mut r);
    assert!(k >= 1);
    std::println!("real tree: {} cpus, pcie ranges {:x?}", n, &r[..k]);
    let mut banks = [MemRange { base: 0, size: 0 }; MAX_BANKS];
    let nb = d.memory(&mut banks);
    std::println!("memory banks in the on-disk tree: {:x?}", &banks[..nb]);
    std::println!(
        "bootargs: {}",
        core::str::from_utf8(d.bootargs()).unwrap_or("?")
    );
}

mod genet_tests {
    use crate::genet::*;

    #[test]
    fn descriptor_words() {
        // 60 bytes: length 60 in 27:16, the default queue tag, CRC appended, one-descriptor frame.
        assert_eq!(tx_status(60), 60 << 16 | 0x3f << 7 | 0x40 | 0x2000 | 0x4000);
        assert_eq!(tx_status(1514) >> 16, 1514);
        assert_eq!(tx_status(0x1fff) >> 16, 0xfff, "lengths are 12 bits");
        assert_eq!(rx_status(2048), 2048 << 16);
    }

    #[test]
    fn received_frames() {
        let good = 64 << 16 | 0x2000 | 0x4000;
        assert_eq!(rx_frame_len(good, 2048), Some(64));
        // Not a whole frame (a middle fragment), or flagged bad.
        assert_eq!(rx_frame_len(64 << 16 | 0x2000, 2048), None);
        assert_eq!(rx_frame_len(64 << 16 | 0x4000, 2048), None);
        for bit in [0x01, 0x02, 0x04, 0x08, 0x10] {
            assert_eq!(rx_frame_len(good | bit, 2048), None, "error bit {bit:#x}");
        }
        // A runt, and one bigger than our buffers.
        assert_eq!(rx_frame_len(13 << 16 | 0x6000, 2048), None);
        assert_eq!(rx_frame_len(14 << 16 | 0x6000, 2048), Some(14));
        assert_eq!(rx_frame_len(3000 << 16 | 0x6000, 2048), None);
        assert_eq!(rx_frame_len(0, 2048), None);
    }

    #[test]
    fn ring_indexes_wrap_like_the_hardware_16_bit_counters() {
        assert_eq!(in_flight(5, 5), 0);
        assert_eq!(in_flight(10, 3), 7);
        assert_eq!(in_flight(2, 0xfffe), 4);
        assert_eq!(in_flight(0, 0), 0);
    }

    #[test]
    fn autonegotiation_picks_the_best_common_mode() {
        let adv = 0x01e1; // 10/100 half and full
        // Both ends have gigabit full: STAT1000 bit 11 is the partner's full duplex.
        assert_eq!(
            resolve(adv, 0x01e1, 0x0300, 0x0800),
            Some(Negotiated {
                mbps: 1000,
                full: true
            })
        );
        // The partner only has gigabit half.
        assert_eq!(
            resolve(adv, 0x01e1, 0x0300, 0x0400),
            Some(Negotiated {
                mbps: 1000,
                full: false
            })
        );
        // We do not advertise gigabit: 100 full.
        assert_eq!(
            resolve(adv, 0x01e1, 0, 0x0800),
            Some(Negotiated {
                mbps: 100,
                full: true
            })
        );
        // Partner: 100 half and 10 full only.
        assert_eq!(
            resolve(adv, 0x00a1, 0x0300, 0),
            Some(Negotiated {
                mbps: 100,
                full: false
            })
        );
        assert_eq!(
            resolve(adv, 0x0061, 0x0300, 0),
            Some(Negotiated {
                mbps: 10,
                full: true
            })
        );
        assert_eq!(
            resolve(adv, 0x0021, 0, 0),
            Some(Negotiated {
                mbps: 10,
                full: false
            })
        );
        // Nothing in common (or no partner abilities at all).
        assert_eq!(resolve(adv, 0x0001, 0x0300, 0), None);
        assert_eq!(resolve(0, 0x01e1, 0, 0x0c00), None);
    }

    #[test]
    fn mac_address_registers() {
        assert_eq!(
            mac_registers([0xdc, 0xa6, 0x32, 0x12, 0x34, 0x56]),
            (0xdca6_3212, 0x3456)
        );
    }
}

#[test]
fn nodes_switched_off_are_noticed() {
    let mut f = Fdt::new();
    f.begin("");
    f.begin("ethernet@7d580000");
    f.str_prop("compatible", "brcm,bcm2711-genet-v5");
    f.str_prop("status", "disabled");
    f.end();
    f.begin("rng@7e104000");
    f.str_prop("compatible", "brcm,bcm2711-rng200");
    f.str_prop("status", "okay");
    f.end();
    f.begin("pcie@7d500000");
    f.str_prop("compatible", "brcm,bcm2711-pcie");
    f.end();
    f.end();
    let b = f.finish();
    let d = b.dtb().unwrap();
    assert!(d.node_disabled(b"ethernet@"));
    assert!(!d.node_disabled(b"rng@"));
    assert!(
        !d.node_disabled(b"pcie@"),
        "no status property means enabled"
    );
    assert!(!d.node_disabled(b"nothing@"));
}

// ---- the ACT LED boot codes ---------------------------------------------------------------

mod ledcode_tests {
    use crate::ledcode::{self, Burst, Interval, Stage};
    use std::vec::Vec;

    fn iv(on: bool, ms: u32) -> Interval {
        Interval { on, ms }
    }

    #[test]
    fn marker_is_one_long_pulse_between_dark() {
        let m: Vec<_> = Burst::marker().collect();
        assert_eq!(m, [iv(false, 500), iv(true, 1000), iv(false, 500)]);
    }

    #[test]
    fn a_stage_is_n_pulses_then_a_pause() {
        let b: Vec<_> = Burst::stage(Stage::Framebuffer, true).collect();
        assert_eq!(
            b,
            [
                iv(true, 100),
                iv(false, 100),
                iv(true, 100),
                iv(false, 100),
                iv(true, 100),
                iv(false, 300)
            ]
        );
        assert_eq!(
            Burst::stage(Stage::Memory, true).collect::<Vec<_>>(),
            [iv(true, 100), iv(false, 300)]
        );
    }

    #[test]
    fn a_failed_stage_is_the_same_count_but_slow() {
        let b: Vec<_> = Burst::stage(Stage::SdCard, false).collect();
        assert_eq!(b.len(), 10);
        assert_eq!(b[0], iv(true, 400));
        assert_eq!(b[1], iv(false, 400));
        assert_eq!(b[9], iv(false, 800));
        assert_eq!(b.iter().filter(|i| i.on).count(), 5);
    }

    #[test]
    fn stages_are_numbered_in_order_with_distinct_names() {
        for (i, s) in Stage::ALL.iter().enumerate() {
            assert_eq!(s.code() as usize, i + 1);
            assert_eq!(Burst::stage(*s, true).filter(|i| i.on).count(), i + 1);
        }
        let mut names: Vec<_> = Stage::ALL.iter().map(|s| s.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Stage::ALL.len());
    }

    #[test]
    fn a_boot_sequence_alternates_and_fits_the_budget() {
        let all: Vec<_> = Stage::ALL.iter().map(|&s| (s, true)).collect();
        let v: Vec<_> = ledcode::sequence(&all).collect();
        // The marker, then 1+2+...+7 = 28 pulses.
        assert_eq!(v.iter().filter(|i| i.on).count(), 1 + 28);
        // The marker's 1 s pulse is the only on-interval over 100 ms.
        assert_eq!(v.iter().filter(|i| i.on && i.ms > 100).count(), 1);
        // Never two equal levels in a row within a burst, and every interval is a visible length.
        assert!(v.iter().all(|i| i.ms >= 100));
        // A good boot adds 2 s (marker) + sum of (200 N + 200) ms for N = 1..7 = 9 s.
        let total: u32 = v.iter().map(|i| i.ms).sum();
        assert_eq!(total, 9000);
    }

    #[test]
    fn bursts_are_bounded_and_do_not_overflow() {
        // A huge code is cut to MAX_PULSES.
        let b = Burst::pulses(255, true);
        assert_eq!(b.count(), 2 * ledcode::MAX_PULSES as usize);
        assert!(Burst::pulses(255, true).total_ms() < 60_000);
        // The iterator ends and stays ended.
        let mut b = Burst::pulses(0, false);
        assert_eq!(b.next(), None);
        assert_eq!(b.next(), None);
        let mut b = Burst::marker();
        for _ in 0..3 {
            b.next();
        }
        assert_eq!(b.next(), None);
        assert_eq!(b.next(), None);
        // The worst case sum, widened: no u32 overflow.
        let worst: u64 = Burst::pulses(255, true).map(|i| i.ms as u64).sum();
        assert_eq!(worst, Burst::pulses(255, true).total_ms() as u64);
    }

    #[test]
    fn total_ms_matches_the_intervals() {
        let b = Burst::stage(Stage::Scheduler, true);
        assert_eq!(b.total_ms(), 6 * 200 + 200);
        assert_eq!(Burst::marker().total_ms(), 2000);
    }

    #[test]
    fn the_option_is_on_unless_switched_off() {
        assert!(ledcode::enabled(None));
        assert!(ledcode::enabled(Some(b"1")));
        assert!(ledcode::enabled(Some(b"")));
        assert!(ledcode::enabled(Some(b"banana")));
        assert!(!ledcode::enabled(Some(b"0")));
        assert!(!ledcode::enabled(Some(b" off ")));
        assert!(!ledcode::enabled(Some(b"NO")));
        assert!(!ledcode::enabled(Some(b"false")));
        // And through the real command line parser.
        let line = b"console=tty1 milk.ledcodes=0 milk.sdwrite=1\n";
        assert!(!ledcode::enabled(crate::cmdline::value(line, "ledcodes")));
    }
}

// ---- the ACT LED crash cycle --------------------------------------------------------------

mod ledcrash_tests {
    use crate::ledcode::Interval;
    use crate::ledcrash::*;
    use std::vec::Vec;

    fn cycle(r: CrashRecord) -> Vec<Interval> {
        CrashCycle::new(r).collect()
    }

    /// xorshift64: the same numbers every run.
    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// The worked example in docs/raspberry-pi.md and scripts/led-decode-test.py.
    fn example() -> CrashRecord {
        CrashRecord::new(4, 0o45, 0x9a3c4, 0x2000a0)
    }

    #[test]
    fn a_cycle_starts_with_two_seconds_of_fast_flashing() {
        let v = cycle(example());
        for (i, iv) in v[..20].iter().enumerate() {
            assert_eq!(iv.on, i % 2 == 0);
            assert!(iv.ms == 100 || (i == 19 && iv.ms == GROUP_GAP_MS));
        }
        let fast: u32 = v[..19].iter().map(|i| i.ms).sum();
        assert_eq!(fast, 1900);
        // Nothing else in the cycle is a pulse as short as the alarm's.
        assert!(v[20..].iter().filter(|i| i.on).all(|i| i.ms >= PULSE_MS));
    }

    #[test]
    fn digits_are_pulses_and_zero_is_one_long_pulse() {
        let r = CrashRecord::new(0, 0, 0, 0);
        let v = cycle(r);
        // 23 zeros: 23 long pulses after the alarm.
        assert_eq!(v[20..].iter().filter(|i| i.on).count(), DIGITS);
        assert!(v[20..].iter().filter(|i| i.on).all(|i| i.ms == ZERO_MS));
        let r = CrashRecord::new(3, 0o12, 0, 0);
        let v = cycle(r);
        // stage 3: three short pulses with 250 ms between and 2.5 s after.
        assert_eq!(
            &v[20..26],
            &[
                Interval { on: true, ms: 150 },
                Interval { on: false, ms: 250 },
                Interval { on: true, ms: 150 },
                Interval { on: false, ms: 250 },
                Interval { on: true, ms: 150 },
                Interval { on: false, ms: GROUP_GAP_MS },
            ]
        );
        // ec 0o12: one pulse, 0.7 s, two pulses, group gap.
        assert_eq!(v[26], Interval { on: true, ms: 150 });
        assert_eq!(v[27], Interval { on: false, ms: DIGIT_GAP_MS });
        assert_eq!(v[31], Interval { on: false, ms: GROUP_GAP_MS });
    }

    #[test]
    fn digits_read_back_in_order() {
        let r = example();
        let d: Vec<u8> = (0..DIGITS).map(|i| r.digit(i)).collect();
        assert_eq!(d[0], 4);
        assert_eq!(&d[1..3], &[4, 5]);
        assert_eq!(&d[3..15], &[0, 0, 0, 0, 0, 2, 3, 2, 1, 7, 0, 4][..]);
        assert_eq!(&d[15..23], &[1, 0, 0, 0, 0, 2, 4, 0][..]);
    }

    #[test]
    fn round_trip_many_values() {
        let mut s = 0x1234_5678_9abc_def1u64;
        let mut cases = std::vec![
            CrashRecord::new(0, 0, 0, 0),
            CrashRecord::new(8, 0o77, u64::MAX, u64::MAX),
            CrashRecord::new(255, 255, u64::MAX, u64::MAX),
            CrashRecord::new(7, 0o70, 0o7777_7777_7777, 0o7777_7777),
            example(),
        ];
        for _ in 0..3000 {
            let a = rng(&mut s);
            let b = rng(&mut s);
            cases.push(CrashRecord::new((a % 9) as u8, (a >> 8) as u8, b, a >> 20));
        }
        for r in cases {
            let v = cycle(r);
            assert_eq!(decode(v.iter().copied()), Some(r), "{r}");
            // A cycle is bounded: under 90 s, and no u32 overflow.
            let total: u64 = v.iter().map(|i| u64::from(i.ms)).sum();
            assert_eq!(total, u64::from(CrashCycle::total_ms(r)));
            assert!(total < 90_000, "{r}: {total} ms");
            // Alternating levels, and every interval is a visible length.
            for (i, iv) in v.iter().enumerate() {
                assert_eq!(iv.on, i % 2 == 0);
                assert!(iv.ms >= 100);
            }
        }
    }

    #[test]
    fn the_worst_cycle_is_under_ninety_seconds() {
        let worst = CrashRecord::new(8, 0o77, u64::MAX, u64::MAX);
        assert!(CrashCycle::total_ms(worst) < 90_000);
        // All sevens is the longest digit; stage 8 the longest stage.
        let sevens = CrashRecord::new(8, 0o77, 0o7777_7777_7777, 0o7777_7777);
        assert_eq!(CrashCycle::total_ms(worst), CrashCycle::total_ms(sevens));
        assert_eq!(CrashCycle::total_ms(sevens), 86_750);
    }

    #[test]
    fn decoding_works_from_the_middle_of_a_repeat() {
        // A video that starts in the middle of one cycle and runs into the next.
        let r = example();
        let v = cycle(r);
        let mut seen: Vec<Interval> = v[37..].to_vec();
        seen.extend(cycle(r));
        assert_eq!(decode(seen.iter().copied()), Some(r));
        // A leading dark and trailing partial cycle are fine too.
        let mut seen = std::vec![Interval { on: false, ms: 4000 }];
        seen.extend(cycle(r));
        seen.extend(&cycle(r)[..50]);
        assert_eq!(decode(seen.iter().copied()), Some(r));
    }

    #[test]
    fn a_phone_video_with_a_frame_of_jitter_still_decodes() {
        // Timing measured from video is off by up to a frame (33 ms) at each edge.
        let mut s = 7u64;
        for _ in 0..500 {
            let a = rng(&mut s);
            let r = CrashRecord::new((a % 9) as u8, (a >> 8) as u8, rng(&mut s), rng(&mut s));
            let v: Vec<Interval> = cycle(r)
                .iter()
                .enumerate()
                .map(|(i, iv)| {
                    // The edge before an interval moves by -33..=33 and the edge after it by the opposite.
                    let j = (rng(&mut s) % 34) as i64;
                    let sign = if i % 2 == 0 { 1 } else { -1 };
                    Interval { on: iv.on, ms: (i64::from(iv.ms) + sign * j) as u32 }
                })
                .collect();
            assert_eq!(decode(v.iter().copied()), Some(r));
        }
    }

    #[test]
    fn a_truncated_or_noisy_cycle_is_not_decoded() {
        let v = cycle(example());
        assert_eq!(decode(v[..v.len() - 2].iter().copied()), None);
        assert_eq!(decode(v[..30].iter().copied()), None);
        assert_eq!(decode(Vec::new()), None);
        // Boot codes (no alarm) are not a crash cycle.
        let boot: Vec<Interval> = crate::ledcode::sequence(&crate::ledcode::Stage::ALL.map(|s| (s, true))).collect();
        assert_eq!(decode(boot.iter().copied()), None);
        // A dropped pulse changes the digit but never panics; garbage never panics.
        let mut s = 99u64;
        for _ in 0..500 {
            let g: Vec<Interval> = (0..200)
                .map(|i| Interval { on: i % 2 == 0, ms: (rng(&mut s) % 4000) as u32 })
                .collect();
            let _ = decode(g.iter().copied());
        }
    }

    #[test]
    fn a_panic_names_its_file_and_line() {
        let r = CrashRecord::panic(5, "kernel/src/rpi/lcd.rs", 412);
        assert_eq!(r.ec, EC_PANIC);
        assert!(r.is_panic());
        assert_eq!(r.pc, 412);
        assert_eq!(r.far, file_hash24("kernel/src/rpi/lcd.rs"));
        assert_ne!(file_hash24("a.rs"), file_hash24("b.rs"));
        assert_eq!(decode(cycle(r).iter().copied()), Some(r));
    }

    #[test]
    fn fields_are_cut_to_their_size() {
        let r = CrashRecord::exception(9, 0xffff_ffff_ffff_ffff, 0xffff_0000_0008_1234, 0xdead_beef_1234_5678);
        assert_eq!(r.stage, 8);
        assert_eq!(r.ec, 0o77);
        assert_eq!(r.pc, 0x0_0008_1234);
        assert_eq!(r.kernel_pc(), 0xffff_0000_0008_1234);
        assert_eq!(r.far, 0x34_5678);
        let esr = 0x25u64 << 26 | 1 << 25 | 0x45;
        assert_eq!(CrashRecord::exception(1, esr, 0, 0).ec, 0o45);
    }

    #[test]
    fn display_is_what_the_decoder_takes() {
        assert_eq!(
            std::format!("{}", example()),
            "stage=4 ec=0o45 pc=0o000002321704 far=0o10000240"
        );
    }

    #[test]
    fn golden_cycle_checksum() {
        // scripts/led-decode-test.py computes the same number from its own encoder.
        let v = cycle(example());
        let sum = v.iter().enumerate().fold(0u64, |a, (i, iv)| (a * 31 + (i as u64 + 1) * u64::from(iv.ms) + u64::from(iv.on)) % 1_000_000_007);
        assert_eq!(v.len(), GOLDEN_LEN);
        assert_eq!(sum, GOLDEN_SUM);
    }
    const GOLDEN_LEN: usize = 120;
    const GOLDEN_SUM: u64 = 925_751_336;
}

#[test]
fn a_mailbox_reply_is_matched_by_channel_and_buffer_not_by_alias() {
    use crate::mailbox::{BUS_ALIASES, Echo, echo};
    let sent = 0xc012_3450 | 8;
    assert_eq!(echo(sent, sent), Echo::Exact);
    // The firmware hands the address back without the alias bits, or with another alias.
    assert_eq!(echo(0x0012_3450 | 8, sent), Echo::SameBuffer);
    assert_eq!(echo(0x4012_3450 | 8, sent), Echo::SameBuffer);
    assert_eq!(echo(0xc012_3460 | 8, sent), Echo::OtherBuffer);
    assert_eq!(echo(0xc012_3450 | 1, sent), Echo::OtherChannel);
    assert_eq!(BUS_ALIASES[0], 0xc000_0000);
    let mut seen = BUS_ALIASES.to_vec();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), BUS_ALIASES.len());
    // Every alias gives the same buffer back through bus_to_phys.
    for a in BUS_ALIASES {
        assert_eq!(mailbox::bus_to_phys(a | 0x12_3450), 0x12_3450);
    }
}

#[cfg(test)]
mod esr_tests {
    use crate::esr::*;
    use std::format;
    use std::string::{String, ToString};

    fn dec(esr: u64, far: u64, ras: bool) -> String {
        Decode { esr, far, ras }.to_string()
    }

    /// xorshift64: the same numbers every run.
    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn exception_classes_by_name() {
        // The values the kernel and the LED codes talk about.
        for (ec, name) in [
            (0x00, "unknown reason"),
            (0x15, "SVC (AArch64)"),
            (0x18, "trapped system register / MSR / MRS"),
            (0x20, "instruction abort, lower EL"),
            (0x21, "instruction abort, same EL"),
            (0x22, "PC alignment fault"),
            (0x24, "data abort, lower EL"),
            (0x25, "data abort, same EL"),
            (0x26, "SP alignment fault"),
            (0x2f, "SError"),
            (0x3c, "BRK (AArch64)"),
            (0x3e, "other"),
        ] {
            assert_eq!(ec_name(ec), name, "EC {ec:#x}");
        }
    }

    #[test]
    fn fields_of_an_esr() {
        let esr = 0x96000045u64; // data abort, same EL... bit 31..26 = 100101
        assert_eq!(ec(esr), 0x25);
        assert_eq!(iss(esr), 0x45);
        assert_eq!(ec(0xbf00_0000), 0x2f);
        assert_eq!(iss(0xffff_ffff), 0x01ff_ffff);
    }

    #[test]
    fn a_translation_fault_on_a_write() {
        // EC 0x25, IL 1, DFSC 0x07 (translation, level 3), WnR 1.
        let esr = 0x25 << 26 | 1 << 25 | 1 << 6 | 0x07;
        let s = dec(esr, 0xdead_beef_0000, true);
        assert!(s.contains("EC=0x25 (data abort, same EL) IL=1"), "{s}");
        assert!(s.contains("translation fault, level 3 on write"), "{s}");
        assert!(s.contains("DFSC=0x07 WnR=1 CM=0 S1PTW=0 EA=0 FnV=0"), "{s}");
        assert!(s.contains("FAR_EL1 = 0x0000deadbeef0000  (address accessed)"), "{s}");
        assert!(!s.contains("ISV=1"), "{s}");
    }

    #[test]
    fn a_data_abort_with_a_valid_syndrome_names_size_and_register() {
        // ISV, SAS = 2 (4 bytes), SRT = x9, SF = 0, WnR = 1, DFSC = 0x10 (synchronous external abort), EA.
        let iss = 1 << 24 | 2 << 22 | 9 << 16 | 1 << 9 | 1 << 6 | 0x10;
        let s = dec(0x25 << 26 | 1 << 25 | iss, 0xfd58_0000, false);
        assert!(s.contains("synchronous external abort on write"), "{s}");
        assert!(s.contains("ISV=1: 4 byte(s), register 9, 32-bit"), "{s}");
        assert!(s.contains("the external bus answered with an error"), "{s}");
        // A 64-bit load into x30 of 8 bytes.
        let iss = 1 << 24 | 3 << 22 | 30 << 16 | 1 << 15 | 0x04;
        let s = dec(0x24 << 26 | 1 << 25 | iss, 0x10, false);
        assert!(s.contains("data abort, lower EL"), "{s}");
        assert!(s.contains("translation fault, level 0 on read"), "{s}");
        assert!(s.contains("8 byte(s), register 30, 64-bit"), "{s}");
    }

    #[test]
    fn far_not_valid_and_cache_maintenance_and_table_walk() {
        let iss = 1 << 10 | 1 << 8 | 1 << 7 | 0x0d;
        let s = dec(0x25 << 26 | iss, 0x1234, false);
        assert!(s.contains("permission fault, level 1 on cache maintenance (stage 1 table walk)"), "{s}");
        assert!(s.contains("FAR_EL1 = not valid"), "{s}");
        assert!(!s.contains("0x1234"), "{s}");
    }

    #[test]
    fn an_instruction_abort() {
        let s = dec(0x21 << 26 | 1 << 25 | 0x0f, 0xffff_0000_0008_0000, false);
        assert!(s.contains("instruction abort: permission fault, level 3"), "{s}");
        assert!(s.contains("IFSC=0x0f S1PTW=0 EA=0 FnV=0"), "{s}");
        assert!(s.contains("FAR_EL1 = 0xffff000000080000  (address fetched)"), "{s}");
        let s = dec(0x20 << 26 | 1 << 10 | 0x04, 0, false);
        assert!(s.contains("FAR_EL1 = not valid"), "{s}");
    }

    #[test]
    fn an_serror_without_ras_and_with_it() {
        // The brief's case: lower EL SError, empty syndrome.
        let s = dec(serror_esr(0), 0, false);
        assert!(s.contains("ESR_EL1 = 0xbe000000  EC=0x2f (SError) IL=1 ISS=0x0000000"), "{s}");
        assert!(s.contains("asynchronous external abort; ELR_EL1 is where the CPU was"), "{s}");
        assert!(s.contains("IDS=0 IESB=0 AET=0 (uncontainable) EA=0 DFSC=0x00 (uncategorized)"), "{s}");
        assert!(s.contains("no RAS extension"), "{s}");
        assert!(s.contains("empty syndrome"), "{s}");
        // With RAS: AET = recoverable (3), EA, DFSC = asynchronous SError (0x11), IESB.
        let iss = 1 << 13 | 3 << 10 | 1 << 9 | 0x11;
        let s = dec(serror_esr(iss), 0, true);
        assert!(s.contains("IDS=0 IESB=1 AET=3 (recoverable state) EA=1 DFSC=0x11 (asynchronous SError interrupt)"), "{s}");
        assert!(!s.contains("no RAS"), "{s}");
        assert!(!s.contains("empty syndrome"), "{s}");
        // IDS = 1: implementation defined.
        let s = dec(serror_esr(1 << 24 | 0x12_3456), 0, false);
        assert!(s.contains("IDS=1: ISS[23:0] = 0x123456 is implementation defined"), "{s}");
        assert!(!s.contains("AET="), "{s}");
    }

    #[test]
    fn aet_and_dfsc_names() {
        assert_eq!(serror_aet(0), "uncontainable");
        assert_eq!(serror_aet(1), "unrecoverable state");
        assert_eq!(serror_aet(2), "restartable state");
        assert_eq!(serror_aet(3), "recoverable state");
        assert_eq!(serror_aet(6), "corrected");
        assert_eq!(serror_aet(4), "reserved");
        assert_eq!(serror_dfsc(0x11), "asynchronous SError interrupt");
        assert_eq!(serror_dfsc(0), "uncategorized");
        assert_eq!(serror_dfsc(0x3f), "reserved");
    }

    #[test]
    fn immediates_and_system_registers() {
        let s = dec(0x15 << 26 | 1 << 25 | 0x1234, 0, false);
        assert!(s.contains("SVC (AArch64)") && s.contains("immediate = 0x1234"), "{s}");
        let s = dec(0x3c << 26 | 1 << 25 | 0x5e1f, 0, false);
        assert!(s.contains("immediate = 0x5e1f"), "{s}");
        // MRS x3, ID_AA64PFR0_EL1 = S3_0_C0_C4_0: op0 3, op1 0, CRn 0, CRm 4, op2 0, Rt 3, read.
        let iss = 3 << 20 | 3 << 5 | 4 << 1 | 1;
        let s = dec(0x18 << 26 | 1 << 25 | iss, 0, false);
        assert!(s.contains("MRS S3_0_C0_C4_0 x3"), "{s}");
    }

    #[test]
    fn ras_levels_and_pending_bits() {
        assert_eq!(pfr0_ras(0x2222), 0);
        assert_eq!(ras_name(pfr0_ras(0x2222)), "none");
        assert_eq!(pfr0_ras(0x1000_2222), 1);
        assert_eq!(ras_name(1), "RAS");
        assert_eq!(ras_name(2), "RAS + RASv1p1");
        assert_eq!(isr_pending(0).to_string(), "none");
        assert_eq!(isr_pending(1 << 8).to_string(), "SError");
        assert_eq!(isr_pending(1 << 8 | 1 << 7).to_string(), "SError IRQ");
        assert_eq!(isr_pending(0xc0 | 1 << 8).to_string(), "SError IRQ FIQ");
        assert_eq!(access_size(0), 1);
        assert_eq!(access_size(3), 8);
    }

    #[test]
    fn fuzz_every_esr_decodes_without_panicking_and_names_its_class() {
        let mut st = 0x9e37_79b9_7f4a_7c15u64;
        for i in 0..200_000u32 {
            let esr = if i < 64 { (i as u64) << 26 | rng(&mut st) & 0x03ff_ffff } else { rng(&mut st) & 0xffff_ffff };
            let far = rng(&mut st);
            for ras in [false, true] {
                let s = format!("{}", Decode { esr, far, ras });
                assert!(s.starts_with("  ESR_EL1 = "), "{s}");
                assert!(s.contains(&format!("EC={:#04x} ({})", ec(esr), ec_name(ec(esr)))), "{s}");
                // Every line is indented, short enough for a 3.5 inch panel's console plus its indent, and no
                // line is empty.
                for l in s.lines() {
                    assert!(l.starts_with("  ") && l.len() > 3 && l.len() < 100, "{l:?}");
                }
            }
            // All 64 fault status codes have some name; never empty.
            assert!(!fault_status(esr & 0x3f).is_empty());
        }
    }

    #[test]
    fn a_table_of_known_dumps() {
        // (esr, far, expected first line) captured from the shapes the kernel prints.
        let table: [(u64, &str); 4] = [
            (0x9600_0045, "  ESR_EL1 = 0x96000045  EC=0x25 (data abort, same EL) IL=1 ISS=0x0000045"),
            (0x8200_000f, "  ESR_EL1 = 0x8200000f  EC=0x20 (instruction abort, lower EL) IL=1 ISS=0x000000f"),
            (0xbe00_0000, "  ESR_EL1 = 0xbe000000  EC=0x2f (SError) IL=1 ISS=0x0000000"),
            (0x5600_0001, "  ESR_EL1 = 0x56000001  EC=0x15 (SVC (AArch64)) IL=1 ISS=0x0000001"),
        ];
        for (esr, first) in table {
            let s = dec(esr, 0, false);
            assert_eq!(s.lines().next().unwrap(), first);
        }
    }
}

#[test]
fn soc_blocks_are_named_by_address_and_do_not_overlap() {
    use crate::soc::{BLOCKS, block};
    assert_eq!(block(0xfd58_0014), Some(("genet", 0x14)));
    assert_eq!(block(0xfe34_0034), Some(("emmc2", 0x34)));
    assert_eq!(block(0xfe10_4024), Some(("rng200", 0x24)));
    assert_eq!(block(0xfe20_0020), Some(("gpio", 0x20)));
    assert_eq!(block(0xfe00_b890), Some(("mailbox", 0x10)));
    assert_eq!(block(0xff84_1104), Some(("gic-dist", 0x104)));
    assert_eq!(block(0x8000_0000), None);
    assert_eq!(block(0xfe34_0100), None);
    for w in BLOCKS.windows(2) {
        assert!(w[0].0 + w[0].1 <= w[1].0, "{:?} overlaps {:?}", w[0], w[1]);
    }
}
