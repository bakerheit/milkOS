//! EL0 smoke test that needs no user build: a few instructions of hand-written user code
//! copied into fresh address spaces. Proves eret to EL0, syscalls, ASID/TTBR0 switching,
//! per-thread FP/SIMD state, fault kills, and that exits give every frame and ASID back.

use alloc::boxed::Box;

use super::fd::FdTable;
use super::uspace::{IMAGE_BASE, Perm, UserSpace};
use super::{SIGSEGV, start, user_frame, wait_kernel_child, wstatus_exited, wstatus_signaled};
use crate::sched::{self, FpState};
use crate::{fs, kprintln, pmm};

core::arch::global_asm!(
    r#"
.section .rodata.user_blob, "a"
.balign 16
.global user_blob_start
user_blob_start:
    // x0 = id, x1 = pattern. Put the pattern in d0, print, then yield 20 times checking
    // d0 survives the other process using d0 too. exit(id), or exit(255) if clobbered.
    .arch_extension fp
    mov     x19, x0
    mov     x20, x1
    fmov    d0, x20
    mov     x8, #64
    mov     x0, #1
    adr     x1, 9f
    mov     x2, #(10f - 9f)
    svc     #0
    mov     x21, #20
1:  mov     x8, #124
    svc     #0
    fmov    x22, d0
    cmp     x22, x20
    b.ne    2f
    subs    x21, x21, #1
    b.ne    1b
    mov     x8, #93
    mov     x0, x19
    svc     #0
2:  mov     x8, #93
    mov     x0, #255
    svc     #0
9:  .ascii  "[el0] hello from user mode\n"
10:
.balign 4
.global user_blob_fault
user_blob_fault:
    mov     x0, #0x10
    ldr     x1, [x0]
    b       .
.global user_blob_end
user_blob_end:
"#
);

unsafe extern "C" {
    static user_blob_start: u8;
    static user_blob_fault: u8;
    static user_blob_end: u8;
}

fn blob() -> (&'static [u8], usize) {
    // SAFETY: linker symbols delimiting the blob in .rodata.
    unsafe {
        let s = &raw const user_blob_start;
        let len = (&raw const user_blob_end).offset_from(s) as usize;
        let fault = (&raw const user_blob_fault).offset_from(s) as usize;
        (core::slice::from_raw_parts(s, len), fault)
    }
}

fn launch(name: &str, entry_off: usize, x0: u64, x1: u64) -> u32 {
    let (code, _) = blob();
    let mut s = UserSpace::new().expect("user space");
    s.map_range(IMAGE_BASE, code.len(), Perm::RX).unwrap();
    s.poke(IMAGE_BASE, code).unwrap();
    s.sync_icache();
    let sp = s.map_stack().unwrap();
    let mut f = user_frame(IMAGE_BASE + entry_off, sp);
    f.x[0] = x0;
    f.x[1] = x1;
    start(super::Launch {
        name,
        ppid: 0,
        pgid: None,
        sid: None,
        space: s,
        fds: FdTable::with_console().unwrap(),
        cwd: fs::root(),
        root_dir: None,
        caps: super::caps::Caps::all(),
        frame: &f,
        tpidr_el0: 0,
        fp: Box::new(FpState::zeroed()),
        sig: super::signal::SigState::new(),
        argv: alloc::vec::Vec::new(),
        exe: alloc::string::String::from(name),
        clear_child_tid: 0,
    })
    .expect("start")
}

/// Pipes between kernel threads: 300 KB through the 64 KiB ring in odd-sized chunks,
/// EOF when the writer goes, EPIPE when the reader goes.
fn pipes() {
    use crate::fs::{OpenFile, abi};
    use alloc::sync::Arc;
    let (r, w) = super::pipe::new(false);
    let r = Arc::new(OpenFile::new(r, abi::O_RDONLY));
    let w = OpenFile::new(w, abi::O_WRONLY);
    const TOTAL: usize = 300_000;
    let writer = sched::spawn("pipe-w", move || {
        let mut sent = 0usize;
        let mut chunk = 1usize;
        while sent < TOTAL {
            let n = chunk.min(TOTAL - sent);
            let data: alloc::vec::Vec<u8> = (sent..sent + n).map(|i| (i * 7) as u8).collect();
            assert!(w.write(&data).unwrap() == n);
            sent += n;
            chunk = chunk * 3 % 9001 + 1;
        }
        drop(w); // last write end: reader sees EOF
    });
    let mut got = 0usize;
    let mut buf = [0u8; 1000];
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for (k, &b) in buf[..n].iter().enumerate() {
            assert!(
                b == ((got + k) * 7) as u8,
                "pipe data corrupted at {}",
                got + k
            );
        }
        got += n;
    }
    writer.join();
    let (r2, w2) = super::pipe::new(false);
    drop(r2);
    let w2 = OpenFile::new(w2, abi::O_WRONLY);
    let e = w2.write(b"x");
    kprintln!(
        "[proc-test] pipe: {got} bytes between threads, EOF after writer closed, write with no reader -> {:?}",
        e.err().map(|e| e.0)
    );
    assert!(got == TOTAL && e == Err(super::pipe::EPIPE));
    let _ = r;
}

pub fn run() {
    pipes();
    // Warm-up run so one-time page-table growth (kernel stacks) is out of the count.
    let w = launch("warmup", 0, 0, 0);
    wait_kernel_child(w);
    sched::reap_all_of(&[w]);
    let before = pmm::stats().free_frames;

    let (_, fault_off) = blob();
    let a = launch("el0-a", 0, 1, 0x1111_2222_3333_4444);
    let b = launch("el0-b", 0, 2, 0x5555_6666_7777_8888);
    let c = launch("el0-fault", fault_off, 0, 0);
    let (sa, sb, sc) = (
        wait_kernel_child(a),
        wait_kernel_child(b),
        wait_kernel_child(c),
    );
    sched::reap_all_of(&[a, b, c]);
    // Other threads (net, tty) run on other CPUs meanwhile and may allocate or free a
    // little: a leak is frames *missing*, and it must still be missing a moment later.
    let mut after = pmm::stats().free_frames;
    if after < before {
        sched::sleep_ms(50);
        sched::reap_all();
        after = pmm::stats().free_frames;
    }
    kprintln!(
        "[proc-test] el0: pid {a} status {sa:#x}, pid {b} status {sb:#x} (FP kept across 40 switches), \
         pid {c} status {sc:#x} (SIGSEGV, core), pmm {before} -> {after}, {} ASIDs in use",
        super::asid::in_use()
    );
    assert!(
        sa == wstatus_exited(1) && sb == wstatus_exited(2),
        "EL0 FP/syscall test failed"
    );
    assert!(
        sc == wstatus_signaled(SIGSEGV) | 0x80,
        "fault did not kill with SIGSEGV"
    );
    assert!(after >= before, "process memory leaked");
    assert!(super::asid::in_use() == 0, "ASID leaked");
    kprintln!("[proc-test] PASS");
}
