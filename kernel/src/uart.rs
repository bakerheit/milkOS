//! PL011 UART receive path: RX interrupt feeding a lock-free ring buffer.
//!
//! (Transmit is still the polled console writer in main.rs.) UART0 is at 0x0900_0000 and
//! raises SPI 1 = INTID 33 on qemu virt (DTB: `pl011@9000000`, `interrupts = <0 1 4>`).
//!
//! The ring has one producer at a time ([`drain`], under `DRAIN`: the IRQ handler, or the
//! slow fallback poll) and one consumer ([`read_byte`], called only by the tty-uart
//! thread), so acquire/release on head and tail is enough on SMP. If the consumer falls
//! behind and the ring fills, new bytes are dropped (counted in [`overruns`]).
//!
//! **Clear, then drain.** qemu's PL011 raises the RX interrupt only when the FIFO goes
//! from empty to non-empty (its trigger level is 1) and drops it when a read empties the
//! FIFO or ICR is written. The handler used to drain first and write ICR after: a byte
//! landing in between (easy when a line arrives as a burst while the handler runs, and
//! qemu's device thread runs in parallel with the vCPU) had its interrupt cleared with
//! the FIFO non-empty, so no further byte ever raised one again: the console wedged with
//! FR = RXFF, RIS.RX = 0. Writing ICR first and then reading until RXFE is set can't lose
//! an edge: anything that arrives after the last read starts a fresh one. (Same order is
//! right for a real PL011's level-triggered FIFO interrupt.) [`poll`] is a slow
//! defence-in-depth re-check that also counts and reports a lost interrupt.

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::exceptions::TrapFrame;
use crate::gic;
use crate::sync::{FnSlot, SpinLock};

const UART_BASE: usize = crate::mmu::phys_to_virt(0x0900_0000);
const UARTDR: usize = 0x00;
const UARTFR: usize = 0x18;
const UARTIFLS: usize = 0x34;
const UARTIMSC: usize = 0x38;
const UARTICR: usize = 0x44;

const FR_RXFE: u32 = 1 << 4;
const INT_RX: u32 = 1 << 4;
const INT_RT: u32 = 1 << 6; // receive timeout: flushes a partly filled FIFO
const INT_ERRORS: u32 = 0x7 << 7 | 1 << 10; // FE, PE, BE, OE

/// SPI 1 on virt.
pub const UART_IRQ: u32 = gic::spi(1);

const RING_SIZE: usize = 256; // power of two
static RING: [AtomicU8; RING_SIZE] = [const { AtomicU8::new(0) }; RING_SIZE];
/// Written only by the IRQ handler.
static HEAD: AtomicUsize = AtomicUsize::new(0);
/// Written only by the reader.
static TAIL: AtomicUsize = AtomicUsize::new(0);
static OVERRUNS: AtomicU64 = AtomicU64::new(0);
/// Times [`poll`] found bytes in the FIFO with no RX interrupt pending.
static RESCUES: AtomicU64 = AtomicU64::new(0);
/// Serialises the producers of the ring (IRQ handler and [`poll`]), across CPUs.
static DRAIN: SpinLock<()> = SpinLock::new(());
/// Called (IRQ context) after new bytes land in the ring; the console tty wakes readers.
static RX_HOOK: FnSlot = FnSlot::new();

/// Install the receive hook (IRQ context, IRQs masked: must not block or allocate).
pub fn set_rx_hook(h: fn()) {
    RX_HOOK.set(h);
}

#[inline]
fn reg_read(off: usize) -> u32 {
    unsafe { read_volatile((UART_BASE + off) as *const u32) }
}
#[inline]
fn reg_write(off: usize, v: u32) {
    unsafe { write_volatile((UART_BASE + off) as *mut u32, v) }
}

fn push(b: u8) {
    let head = HEAD.load(Ordering::Relaxed);
    let tail = TAIL.load(Ordering::Acquire);
    if head.wrapping_sub(tail) >= RING_SIZE {
        OVERRUNS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    RING[head % RING_SIZE].store(b, Ordering::Relaxed);
    HEAD.store(head.wrapping_add(1), Ordering::Release);
}

/// Next received byte, or `None` if the buffer is empty. Never blocks.
pub fn read_byte() -> Option<u8> {
    let tail = TAIL.load(Ordering::Relaxed);
    if tail == HEAD.load(Ordering::Acquire) {
        return None;
    }
    let b = RING[tail % RING_SIZE].load(Ordering::Relaxed);
    TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some(b)
}

/// Bytes waiting in the ring.
pub fn available() -> usize {
    HEAD.load(Ordering::Acquire)
        .wrapping_sub(TAIL.load(Ordering::Acquire))
}

/// Bytes dropped because the ring was full.
pub fn overruns() -> u64 {
    OVERRUNS.load(Ordering::Relaxed)
}

/// Clear the RX / timeout / error interrupts, *then* move every byte in the FIFO into
/// the ring (see the module docs for why that order). Returns how many bytes moved.
fn drain() -> usize {
    let _g = DRAIN.lock();
    reg_write(UARTICR, INT_RX | INT_RT | INT_ERRORS);
    let mut n = 0;
    while reg_read(UARTFR) & FR_RXFE == 0 {
        push((reg_read(UARTDR) & 0xff) as u8);
        n += 1;
    }
    n
}

fn on_irq(_irq: u32, _frame: &mut TrapFrame) {
    drain();
    if let Some(h) = RX_HOOK.get::<fn()>() {
        h();
    }
}

/// Slow fallback (the tty-uart thread calls it about once a second when idle): if the
/// FIFO holds bytes but no RX interrupt is pending, an interrupt was lost; take the
/// bytes anyway, count it and say so once. Returns true if it moved any bytes.
pub fn poll() -> bool {
    const UARTMIS: usize = 0x40;
    if reg_read(UARTFR) & FR_RXFE != 0 || reg_read(UARTMIS) & (INT_RX | INT_RT) != 0 {
        return false; // nothing waiting, or the IRQ is on its way
    }
    if drain() == 0 {
        return false;
    }
    if RESCUES.fetch_add(1, Ordering::Relaxed) == 0 {
        crate::kprintln!("[uart] RX bytes waiting with no RX interrupt: recovered by poll");
    }
    if let Some(h) = RX_HOOK.get::<fn()>() {
        h();
    }
    true
}

/// Times [`poll`] had to rescue bytes (0 unless an RX interrupt was lost).
pub fn rescues() -> u64 {
    RESCUES.load(Ordering::Relaxed)
}

/// Hook up the RX interrupt. GIC must be initialised; IRQs still need unmasking.
pub fn init() {
    reg_write(UARTIMSC, 0); // quiet while we set up
    reg_write(UARTIFLS, 0); // RX FIFO trigger at 1/8 full; RT interrupt covers the rest
    reg_write(UARTICR, 0x7ff);
    gic::register(UART_IRQ, on_irq).expect("uart IRQ registration");
    gic::enable(UART_IRQ).expect("uart IRQ enable");
    reg_write(UARTIMSC, INT_RX | INT_RT);
}

/// Smoke test: echo everything received back to the console (CR becomes CR LF).
pub fn echo_pending() {
    use core::fmt::Write;
    while let Some(b) = read_byte() {
        let mut w = crate::console_writer();
        let _ = match b {
            b'\r' => w.write_str("\n"),
            0x7f | 0x08 => w.write_str("\x08 \x08"),
            _ => w.write_char(b as char),
        };
    }
}
