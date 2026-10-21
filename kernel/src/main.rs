#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::write_volatile;

mod display;
mod dtb;
mod exceptions;
mod fbcon;
mod font;
mod font_data;
mod fs;
mod fwcfg;
mod gfx;
mod gic;
mod heap;
mod mem;
mod memtest;
mod mmu;
mod pmm;
mod ramfb;
mod selftest;
mod sink;
mod splash;
mod sync;
mod timer;
mod uart;

global_asm!(include_str!("boot.S"));

/// PL011 UART on qemu's virt machine.
const UART0: *mut u32 = 0x0900_0000 as *mut u32;

struct Uart;

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                unsafe { write_volatile(UART0, b'\r' as u32) };
            }
            unsafe { write_volatile(UART0, b as u32) };
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        let _ = core::fmt::Write::write_fmt(&mut $crate::console_writer(), format_args!($($arg)*));
        let _ = core::fmt::Write::write_str(&mut $crate::console_writer(), "\n");
    }};
}

/// Writer behind `kprintln!`: UART always, plus the screen once `display::init` has run.
pub fn console_writer() -> impl Write {
    sink::Sink
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    // Vectors first, so a fault anywhere after this gets a crash dump.
    exceptions::init();
    // MMU + caches before anything takes a lock (atomics hang on Device memory).
    mem::init();
    display::init();
    kprintln!("milkOS 0.0.1 booting...");
    kprintln!("hello from the kernel");

    gic::init();
    timer::init();
    uart::init();
    sync::irq_enable();
    selftest::run();
    memtest::run();
    fs::init();
    fs::selftest::run();

    loop {
        uart::echo_pending();
        unsafe { core::arch::asm!("wfi") };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n*** KERNEL PANIC ***\n{info}");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
