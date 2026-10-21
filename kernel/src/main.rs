#![no_std]
#![no_main]

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::write_volatile;

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

pub fn console_writer() -> impl Write {
    Uart
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    kprintln!("milkOS 0.0.1 booting...");
    kprintln!("hello from the kernel");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n*** KERNEL PANIC ***\n{info}");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
