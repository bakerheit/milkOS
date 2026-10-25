//! The green ACT LED (GPIO 42 on the Pi 4 B, active high): a sign of life that needs nothing
//! but the GPIO block, for a board with no serial cable and a screen that may not light up.
//!
//! * [`on`], first thing in `kmain`: the LED is on from the first instruction of the kernel.
//! * [`start_codes`] and [`stage`]: boot progress codes (the start marker, then N pulses per
//!   milestone; the pattern language is `rpikit::ledcode`, how to read it is docs/raspberry-pi.md).
//!   Each burst is played by busy-waiting on the generic counter, so it needs no allocator,
//!   timer interrupt or scheduler, and adds about 9 s to a whole boot (`milk.ledcodes=0` turns
//!   the codes off; the `led: stage N name` log lines stay).
//! * [`finish`]: the boot is done, solid on.
//! * [`blink_forever`], from a crash: fast flashing (about 5 Hz), so "on" means "running",
//!   "flashing" means "crashed" even when nothing reached the screen.
//!
//! GPFSEL4 (+0x10) holds the function of pins 40..49 in 3-bit fields (pin 42 is bits 8:6, 001 =
//! output); GPSET1 (+0x20) / GPCLR1 (+0x2c) set / clear pins 32..53 (pin 42 is bit 10).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use rpikit::ledcode::{self, Burst, Stage};

use super::Timeout;
use crate::mmu::phys_to_virt;
use crate::platform::periph;

const GPIO: usize = phys_to_virt(periph(0x20_0000));
const GPFSEL4: usize = 0x10;
const GPSET1: usize = 0x20;
const GPCLR1: usize = 0x2c;
const PIN_BIT: u32 = 1 << (42 - 32);

fn reg_write(off: usize, v: u32) {
    // SAFETY: GPIO registers, mapped as Device memory (platform.rs).
    unsafe { write_volatile((GPIO + off) as *mut u32, v) }
}

fn make_output() {
    // SAFETY: as above; read-modify-write of the function select word for pins 40..49.
    unsafe {
        let p = (GPIO + GPFSEL4) as *mut u32;
        let v = read_volatile(p);
        write_volatile(p, v & !(0b111 << 6) | (0b001 << 6));
    }
}

pub fn on() {
    make_output();
    reg_write(GPSET1, PIN_BIT);
}

/// Are the codes being shown (`milk.ledcodes`, default on)? Set by [`start_codes`].
static CODES: AtomicBool = AtomicBool::new(false);
/// A crash took the LED over: no code is played any more.
static CRASHED: AtomicBool = AtomicBool::new(false);

fn wait_ms(ms: u32) {
    let t = Timeout::ms(u64::from(ms));
    while !t.expired() {}
}

fn play(burst: Burst) {
    for iv in burst {
        if CRASHED.load(Ordering::Relaxed) {
            return;
        }
        reg_write(if iv.on { GPSET1 } else { GPCLR1 }, PIN_BIT);
        wait_ms(iv.ms);
    }
}

/// Read `milk.ledcodes`, show the start marker and the first milestone (memory up). Call once,
/// right after `mem::init` (the device tree is reachable from there on).
pub fn start_codes() {
    let mut buf = [0u8; 8];
    let value = crate::fwcfg::find_file("opt/milkos/ledcodes")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
        .map(|f| &buf[..f.size as usize]);
    let on = ledcode::enabled(value);
    CODES.store(on, Ordering::Relaxed);
    if on {
        play(Burst::marker());
    }
    stage(Stage::Memory, true);
}

/// A milestone was reached (`ok`), or its bounded wait gave up / the device is not there (`!ok`).
/// Logs `led: stage N name`, and plays the burst unless the codes are off.
pub fn stage(stage: Stage, ok: bool) {
    crate::kprintln!(
        "led: stage {} {}{}",
        stage.code(),
        stage.name(),
        if ok { "" } else { " (failed)" }
    );
    if CODES.load(Ordering::Relaxed) {
        play(Burst::stage(stage, ok));
    }
}

/// The boot is done (the desktop is up, or there is no desktop to wait for): solid on.
pub fn finish() {
    crate::kprintln!("led: done, solid on");
    on();
}

/// Flash the LED until the end of time. Never returns.
pub fn blink_forever() -> ! {
    CRASHED.store(true, Ordering::Relaxed);
    make_output();
    loop {
        reg_write(GPSET1, PIN_BIT);
        let t = Timeout::ms(100);
        while !t.expired() {}
        reg_write(GPCLR1, PIN_BIT);
        let t = Timeout::ms(100);
        while !t.expired() {}
    }
}
