//! SPI0 and the GPIO pins the 3.5-inch display kit uses (docs/raspberry-pi.md, "3.5-inch SPI
//! display"), written from the BCM2835/BCM2711 peripheral documentation: polled, no DMA, no
//! interrupts, every wait bounded.
//!
//! SPI0 registers (offset 0x20_4000 from the peripheral base): CS (+0x00: chip select number in
//! bits 1:0, clock mode, FIFO clear bits 5:4, transfer active TA bit 7, DONE bit 16, RXD bit 17,
//! TXD bit 18), FIFO (+0x04: 16 bytes each way, one register), CLK (+0x08: the clock divider; the
//! bit clock is the core clock divided by it, a power of two in practice, and 0 means 65536).
//!
//! A transfer keeps at most 16 bytes in flight (written but not yet read back): the TX FIFO then
//! never overflows and the RX FIFO, which fills with the answers, never does either. Mode 0
//! (clock idle low, data sampled on the rising edge), chip select active low, as the touch chip
//! wants and most panels; a panel may ask for another mode (CS bit 3 is the clock polarity, bit 2
//! the phase: mode 3 is both set). The GPIO function select registers hold 3 bits per pin (ALT0 is 0b100);
//! the BCM2711 has its own pull up/down registers (2 bits per pin at GPIO + 0xe4).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, Ordering};

use super::Timeout;
use crate::mmu::phys_to_virt;
use crate::platform::periph;

const GPIO: usize = phys_to_virt(periph(0x20_0000));
const SPI0: usize = phys_to_virt(periph(0x20_4000));

const GPFSEL0: usize = 0x00;
const GPSET0: usize = 0x1c;
const GPCLR0: usize = 0x28;
const GPLEV0: usize = 0x34;
const GPPUPPDN0: usize = 0xe4;

const CS: usize = 0x00;
const FIFO: usize = 0x04;
const CLK: usize = 0x08;

const CS_CPHA: u32 = 1 << 2;
const CS_CPOL: u32 = 1 << 3;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_TA: u32 = 1 << 7;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17;
const CS_TXD: u32 = 1 << 18;

/// The header pins the options may name: GPIO 2..=27 (the ones on the 40-pin header that are
/// not the ID EEPROM's).
pub fn header_pin(n: i64) -> Option<u8> {
    (2..=27).contains(&n).then_some(n as u8)
}

#[derive(Clone, Copy)]
pub enum Func {
    Input = 0,
    Output = 1,
    Alt0 = 4,
}

#[derive(Clone, Copy)]
pub enum Pull {
    Up = 1,
}

fn gpio_read_reg(off: usize) -> u32 {
    // SAFETY: GPIO registers, mapped as Device memory (platform.rs).
    unsafe { read_volatile((GPIO + off) as *const u32) }
}

fn gpio_write_reg(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((GPIO + off) as *mut u32, v) }
}

/// Set the function of header pin `pin` (0..=27).
pub fn set_function(pin: u8, f: Func) {
    let off = GPFSEL0 + 4 * (pin as usize / 10);
    let sh = 3 * (pin as u32 % 10);
    let v = gpio_read_reg(off);
    gpio_write_reg(off, (v & !(0b111 << sh)) | ((f as u32) << sh));
}

pub fn set_pull(pin: u8, p: Pull) {
    let off = GPPUPPDN0 + 4 * (pin as usize / 16);
    let sh = 2 * (pin as u32 % 16);
    let v = gpio_read_reg(off);
    gpio_write_reg(off, (v & !(0b11 << sh)) | ((p as u32) << sh));
}

/// Drive an output pin.
pub fn write(pin: u8, high: bool) {
    gpio_write_reg(if high { GPSET0 } else { GPCLR0 }, 1 << pin);
}

/// The level of a pin.
pub fn read(pin: u8) -> bool {
    gpio_read_reg(GPLEV0) & (1 << pin) != 0
}

fn spi_read(off: usize) -> u32 {
    // SAFETY: SPI0 registers, Device memory.
    unsafe { read_volatile((SPI0 + off) as *const u32) }
}

fn spi_write(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((SPI0 + off) as *mut u32, v) }
}

/// The core clock the divider divides, in Hz (set by `lcd::init` from the firmware's answer, or
/// its fallback: the Pi 4's `core_freq` default, see `lcd::FALLBACK_CORE_HZ`).
static CORE_HZ: AtomicU32 = AtomicU32::new(500_000_000);

/// The core clock the dividers were computed from, Hz.
pub fn core_hz() -> u32 {
    CORE_HZ.load(Ordering::Relaxed)
}

pub fn set_core_hz(hz: u32) {
    CORE_HZ.store(hz.max(1_000_000), Ordering::Relaxed);
}

/// The clock divider for a bit clock of at most `hz`: even, 2..=65534, rounded up (so the clock
/// is never faster than asked).
pub fn divider_for(core_hz: u32, hz: u32) -> u32 {
    let hz = hz.max(1);
    let d = core_hz.div_ceil(hz).max(2);
    (d + (d & 1)).min(65534)
}

/// Put the header pins on SPI0 (ALT0): GPIO 7 CE1, 8 CE0, 9 MISO, 10 MOSI, 11 SCLK.
pub fn connect_pins() {
    for pin in 7..=11 {
        set_function(pin, Func::Alt0);
    }
}

/// Is there an SPI0 controller that answers? Its clock register keeps what is written, and the
/// transmit FIFO of an idle controller has room. (Unimplemented or absent hardware reads as 0.)
pub fn present() -> bool {
    let old = spi_read(CLK);
    // (The divider's lowest bit is not kept: the values are even.)
    spi_write(CLK, 0x0064);
    let a = spi_read(CLK) & 0xfffe == 0x0064;
    spi_write(CLK, 0x00c8);
    let b = spi_read(CLK) & 0xfffe == 0x00c8;
    spi_write(CLK, old);
    a && b && spi_read(CS) & CS_TXD != 0
}

/// Stop whatever the controller was doing: clear the FIFOs, TA off.
pub fn reset() {
    spi_write(CS, CS_CLEAR_TX | CS_CLEAR_RX);
}

/// The CS register bits for SPI mode `mode` (0..=3: bit 1 is the clock polarity, bit 0 the phase).
const fn mode_bits(mode: u8) -> u32 {
    (if mode & 2 != 0 { CS_CPOL } else { 0 }) | (if mode & 1 != 0 { CS_CPHA } else { 0 })
}

/// One full duplex transfer in SPI mode 0 on chip select `cs` (0 or 1) with clock divider `cdiv`:
/// see [`transfer_mode`].
pub fn transfer(cs: u8, cdiv: u32, tx: &[u8], rx: Option<&mut [u8]>) -> bool {
    transfer_mode(cs, cdiv, 0, tx, rx)
}

/// One full duplex transfer on chip select `cs` (0 or 1) with clock divider `cdiv` in SPI mode
/// `mode` (0..=3): send `tx`, store the bytes that come back in `rx` (the same length) if given.
/// False if the controller did not finish within a generous multiple (six times: the core clock
/// may be running below the rate the divider was computed from) of the time the bytes need on the
/// wire.
pub fn transfer_mode(cs: u8, cdiv: u32, mode: u8, tx: &[u8], rx: Option<&mut [u8]>) -> bool {
    transfer_capped(cs, cdiv, mode, tx, rx, u64::MAX)
}

/// [`transfer_mode`] that never waits longer than `cap_us` microseconds, whatever the wire time
/// says (a caller with a time budget of its own: the early display bring-up, the print path).
pub fn transfer_capped(
    cs: u8,
    cdiv: u32,
    mode: u8,
    tx: &[u8],
    mut rx: Option<&mut [u8]>,
    cap_us: u64,
) -> bool {
    let n = tx.len();
    if n == 0 {
        return true;
    }
    if let Some(r) = rx.as_deref()
        && r.len() < n
    {
        return false;
    }
    let wire_us = n as u64 * 8 * cdiv as u64 * 1_000_000 / CORE_HZ.load(Ordering::Relaxed) as u64;
    let deadline = Timeout::us((2000 + 6 * wire_us).min(cap_us));
    let cs_bits = (cs & 3) as u32 | mode_bits(mode);
    spi_write(CLK, cdiv & 0xffff);
    spi_write(CS, cs_bits | CS_CLEAR_TX | CS_CLEAR_RX);
    spi_write(CS, cs_bits | CS_TA);
    let (mut sent, mut got) = (0usize, 0usize);
    while sent < n && sent < 16 {
        spi_write(FIFO, tx[sent] as u32);
        sent += 1;
    }
    let mut ok = true;
    while got < n {
        if spi_read(CS) & CS_RXD != 0 {
            let b = spi_read(FIFO) as u8;
            if let Some(r) = rx.as_deref_mut() {
                r[got] = b;
            }
            got += 1;
            if sent < n {
                spi_write(FIFO, tx[sent] as u32);
                sent += 1;
            }
        } else if deadline.expired() {
            ok = false;
            break;
        }
    }
    if ok {
        // The last byte has been read back, so it has been shifted out; DONE confirms it.
        while spi_read(CS) & CS_DONE == 0 {
            if deadline.expired() {
                ok = false;
                break;
            }
        }
    }
    if !ok && FAILED_LOGGED.fetch_add(1, Ordering::Relaxed) < 4 {
        crate::kprintln!(
            "spi: a transfer of {n} bytes on CE{cs} (divider {cdiv}) timed out after {sent} sent and {got} read back: CS {:#x} CLK {:#x}",
            spi_read(CS),
            spi_read(CLK)
        );
    }
    spi_write(CS, CS_CLEAR_TX | CS_CLEAR_RX);
    ok
}

/// Failed transfers logged so far (a missing panel would otherwise fill the log).
static FAILED_LOGGED: AtomicU32 = AtomicU32::new(0);

/// SPI0's CS and CLK registers, for logs.
pub fn registers() -> (u32, u32) {
    (spi_read(CS), spi_read(CLK))
}
