//! Loader for static aarch64 ELF64 executables, and the initial user stack in the Linux
//! layout (argc, argv, envp, auxv), so a static Linux binary's `_start` finds what it
//! expects.
//!
//! * `ET_EXEC`: segments go where they are linked; they must lie in the user range
//!   (`mmu::USER_BASE`..`mmu::USER_END`, all of TTBR0: the kernel is in the higher half).
//!   That includes Linux's usual 0x400000 and milkOS's 0x100_0040_0000.
//! * `ET_DYN` without `PT_INTERP` (static-pie): loaded at `IMAGE_BASE`; the program
//!   relocates itself.
//! * Anything with `PT_INTERP` (dynamically linked) is refused.

use alloc::string::String;
use alloc::vec::Vec;

use super::errno::*;
use super::uspace::{IMAGE_BASE, Perm, UserSpace, user_range_ok};
use crate::mmu::PAGE_SIZE;

const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_AARCH64: u16 = 183;
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PF_X: u32 = 1;
const PF_W: u32 = 2;

/// Where the image ended up.
pub struct Image {
    pub entry: usize,
    pub phdr: usize,
    pub phnum: usize,
    pub phent: usize,
    /// One past the highest loaded byte (brk starts at the next page).
    pub end: usize,
}

fn u16_at(d: &[u8], o: usize) -> KResult<u16> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(ENOEXEC)
}
fn u32_at(d: &[u8], o: usize) -> KResult<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or(ENOEXEC)
}
fn u64_at(d: &[u8], o: usize) -> KResult<usize> {
    d.get(o..o + 8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize)
        .ok_or(ENOEXEC)
}

struct Phdr {
    kind: u32,
    flags: u32,
    offset: usize,
    vaddr: usize,
    filesz: usize,
    memsz: usize,
}

/// Why an image was refused, for the log (execve itself just returns ENOEXEC).
pub fn check(data: &[u8]) -> Result<(), &'static str> {
    if data.len() < 64 || &data[..4] != b"\x7fELF" {
        return Err("not an ELF file");
    }
    if data[4] != 2 || data[5] != 1 {
        return Err("not ELF64 little-endian");
    }
    if u16_at(data, 18) != Ok(EM_AARCH64) {
        return Err("not an aarch64 program");
    }
    match u16_at(data, 16) {
        Ok(ET_EXEC) | Ok(ET_DYN) => Ok(()),
        _ => Err("not an executable"),
    }
}

/// Map the PT_LOAD segments of `data` into `space`.
pub fn load(space: &mut UserSpace, data: &[u8]) -> KResult<Image> {
    if let Err(why) = check(data) {
        crate::kprintln!("[exec] refused: {why}");
        return Err(ENOEXEC);
    }
    let etype = u16_at(data, 16)?;
    let entry = u64_at(data, 24)?;
    let phoff = u64_at(data, 32)?;
    let phent = u16_at(data, 54)? as usize;
    let phnum = u16_at(data, 56)? as usize;
    if phent < 56 || phnum == 0 || phnum > 64 {
        return Err(ENOEXEC);
    }
    let mut phdrs = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let o = phoff + i * phent;
        phdrs.push(Phdr {
            kind: u32_at(data, o)?,
            flags: u32_at(data, o + 4)?,
            offset: u64_at(data, o + 8)?,
            vaddr: u64_at(data, o + 16)?,
            filesz: u64_at(data, o + 32)?,
            memsz: u64_at(data, o + 40)?,
        });
    }
    if phdrs.iter().any(|p| p.kind == PT_INTERP) {
        crate::kprintln!("[exec] refused: dynamically linked (PT_INTERP); only static programs");
        return Err(ENOEXEC);
    }
    let bias = if etype == ET_DYN {
        let lowest = phdrs
            .iter()
            .filter(|p| p.kind == PT_LOAD)
            .map(|p| p.vaddr)
            .min()
            .ok_or(ENOEXEC)?;
        IMAGE_BASE - (lowest & !(PAGE_SIZE - 1))
    } else {
        0
    };

    let mut end = 0;
    let zeros = [0u8; 512];
    for p in phdrs.iter().filter(|p| p.kind == PT_LOAD && p.memsz > 0) {
        let va = p.vaddr.checked_add(bias).ok_or(ENOEXEC)?;
        if !user_range_ok(va, p.memsz) {
            crate::kprintln!(
                "[exec] refused: segment at {va:#x} is outside the user range ({:#x}..); \
                 link user programs at {IMAGE_BASE:#x}",
                crate::mmu::USER_BASE
            );
            return Err(ENOEXEC);
        }
        if p.filesz > p.memsz
            || p.offset
                .checked_add(p.filesz)
                .is_none_or(|e| e > data.len())
        {
            return Err(ENOEXEC);
        }
        let perm = Perm {
            write: p.flags & PF_W != 0,
            exec: p.flags & PF_X != 0,
        };
        space.map_range(va, p.memsz, perm)?;
        space.poke(va, &data[p.offset..p.offset + p.filesz])?;
        // .bss: pages are fresh (zeroed), but a page shared with an earlier segment may not
        // be, so clear the tail explicitly.
        let mut z = va + p.filesz;
        while z < va + p.memsz {
            let n = zeros.len().min(va + p.memsz - z);
            space.poke(z, &zeros[..n])?;
            z += n;
        }
        end = end.max(va + p.memsz);
    }
    if end == 0 {
        return Err(ENOEXEC);
    }
    let phdr = match phdrs.iter().find(|p| p.kind == PT_PHDR) {
        Some(p) => p.vaddr + bias,
        None => phdrs
            .iter()
            .find(|p| p.kind == PT_LOAD && p.offset <= phoff && phoff < p.offset + p.filesz)
            .map_or(0, |p| p.vaddr + bias + (phoff - p.offset)),
    };
    space.sync_icache();
    Ok(Image {
        entry: entry + bias,
        phdr,
        phnum,
        phent,
        end,
    })
}

// auxv tags
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;
/// HWCAP_FP | HWCAP_ASIMD: FP/SIMD state is saved per thread.
const HWCAP: u64 = 0b11;

/// Build the initial stack below `top`: strings, then (16-byte aligned) argc, argv[],
/// NULL, envp[], NULL, auxv pairs, AT_NULL. Returns the initial sp.
/// The ids the new image runs with, for the auxv (AT_UID, AT_EUID, AT_GID, AT_EGID) and
/// AT_SECURE: set when the exec changed ids (a set-user-ID / set-group-ID program) or real
/// and effective ids differ, so libc ignores LD_* style environment variables and the like.
pub struct AuxIds {
    pub uid: u32,
    pub euid: u32,
    pub gid: u32,
    pub egid: u32,
    pub secure: bool,
}

impl AuxIds {
    pub fn of(c: &super::cred::Cred, setid_exec: bool) -> AuxIds {
        AuxIds {
            uid: c.ruid,
            euid: c.euid,
            gid: c.rgid,
            egid: c.egid,
            secure: setid_exec || c.ruid != c.euid || c.rgid != c.egid,
        }
    }
}

pub fn build_stack(
    space: &UserSpace,
    top: usize,
    argv: &[String],
    envp: &[String],
    img: &Image,
    execfn: &str,
    ids: &AuxIds,
) -> KResult<usize> {
    let mut sp = top;
    let push_bytes = |b: &[u8], sp: &mut usize| -> KResult<usize> {
        *sp -= b.len();
        space.poke(*sp, b)?;
        Ok(*sp)
    };
    // 16 "random" bytes for AT_RANDOM (stack canaries, hash seeds). Not cryptographic.
    let mut rnd = [0u8; 16];
    let mut x = crate::timer::counter() ^ 0x9e37_79b9_7f4a_7c15;
    for c in rnd.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        c.copy_from_slice(&x.to_le_bytes());
    }
    let random = push_bytes(&rnd, &mut sp)?;
    let cstr = |s: &str, sp: &mut usize| -> KResult<usize> {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        push_bytes(&v, sp)
    };
    let execfn_p = cstr(execfn, &mut sp)?;
    let mut envp_p = Vec::new();
    for e in envp.iter().rev() {
        envp_p.push(cstr(e, &mut sp)?);
    }
    envp_p.reverse();
    let mut argv_p = Vec::new();
    for a in argv.iter().rev() {
        argv_p.push(cstr(a, &mut sp)?);
    }
    argv_p.reverse();

    let auxv: [(u64, u64); 17] = [
        (AT_PHDR, img.phdr as u64),
        (AT_PHENT, img.phent as u64),
        (AT_PHNUM, img.phnum as u64),
        (AT_PAGESZ, PAGE_SIZE as u64),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, img.entry as u64),
        (AT_UID, ids.uid as u64),
        (AT_EUID, ids.euid as u64),
        (AT_GID, ids.gid as u64),
        (AT_EGID, ids.egid as u64),
        (AT_HWCAP, HWCAP),
        (AT_CLKTCK, crate::timer::TICK_HZ),
        (AT_SECURE, ids.secure as u64),
        (AT_RANDOM, random as u64),
        (AT_EXECFN, execfn_p as u64),
        (AT_NULL, 0),
    ];
    let words = 1 + argv_p.len() + 1 + envp_p.len() + 1 + auxv.len() * 2;
    if top - sp + words * 8 > super::uspace::STACK_INIT / 2 {
        return Err(E2BIG);
    }
    sp = (sp - words * 8) & !15;
    let mut block: Vec<u64> = Vec::with_capacity(words);
    block.push(argv_p.len() as u64);
    block.extend(argv_p.iter().map(|&p| p as u64));
    block.push(0);
    block.extend(envp_p.iter().map(|&p| p as u64));
    block.push(0);
    for (k, v) in auxv {
        block.push(k);
        block.push(v);
    }
    let bytes: Vec<u8> = block.iter().flat_map(|w| w.to_le_bytes()).collect();
    space.poke(sp, &bytes)?;
    Ok(sp)
}
