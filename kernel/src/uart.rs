//! PL011 UART receive path: RX interrupt feeding a lock-free ring buffer.
//!
//! (Transmit is still the polled console writer in main.rs.) UART0 is at 0x0900_0000 and
//! raises SPI 1 = INTID 33 on qemu virt (DTB: `pl011@9000000`, `interrupts = <0 1 4>`).
//!
//! The ring is single-producer (the IRQ handler) / single-consumer ([`read_byte`]), so it
//! needs no lock and only plain atomic loads/stores. If the consumer falls behind and the
//! ring fills, new bytes are dropped (counted in [`overruns`]).

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::exceptions::TrapFrame;
use crate::gic;

const UART_BASE: usize = 0x0900_0000;
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
        OVERRUNS.store(OVERRUNS.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
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

fn on_irq(_irq: u32, _frame: &mut TrapFrame) {
    // Drain the FIFO, then clear the (level) RX / timeout / error sources.
    while reg_read(UARTFR) & FR_RXFE == 0 {
        push((reg_read(UARTDR) & 0xff) as u8);
    }
    reg_write(UARTICR, INT_RX | INT_RT | INT_ERRORS);
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
