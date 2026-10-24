//! The green ACT LED (GPIO 42 on the Pi 4 B, active high): a sign of life that needs nothing
//! but the GPIO block, for a board with no serial cable and a screen that may not light up.
//!
//! * [`on`], first thing in `kmain`: the LED stays on while the kernel runs.
//! * [`blink_forever`], from a crash: fast flashing (about 5 Hz), so "on" means "running",
//!   "flashing" means "crashed" even when nothing reached the screen.
//!
//! GPFSEL4 (+0x10) holds the function of pins 40..49 in 3-bit fields (pin 42 is bits 8:6, 001 =
//! output); GPSET1 (+0x20) / GPCLR1 (+0x2c) set / clear pins 32..53 (pin 42 is bit 10).

use core::ptr::{read_volatile, write_volatile};

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

/// Flash the LED until the end of time. Never returns.
pub fn blink_forever() -> ! {
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
