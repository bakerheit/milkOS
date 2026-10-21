#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::write_volatile;

mod cursor;
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
mod input;
mod mem;
mod memtest;
mod mmu;
mod pmm;
mod ramfb;
mod sched;
mod selftest;
mod sink;
mod splash;
mod sync;
mod timer;
mod uart;
mod virtio;

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
        // IRQs off for the whole line so threads don't interleave mid-line.
        $crate::sync::without_irqs(|| {
            let _ = core::fmt::Write::write_fmt(&mut $crate::console_writer(), format_args!($($arg)*));
            let _ = core::fmt::Write::write_str(&mut $crate::console_writer(), "\n");
        })
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
    virtio::init();
    cursor::init();
    sync::irq_enable();
    selftest::run();
    memtest::run();
    fs::init();
    fs::selftest::run();
    sched::init();
    sched::selftest::run();

    loop {
        uart::echo_pending();
        input_demo();
        unsafe { core::arch::asm!("wfi") };
    }
}

/// Demo until the window server exists: typed text echoes on the console, the mouse moves
/// the cursor, buttons and scrolling are logged to the serial port only.
fn input_demo() {
    while let Some(ev) = input::poll() {
        match ev {
            input::Event::PointerMove { x, y } => cursor::move_to(x, y),
            input::Event::KeyDown(k) => match k.text() {
                Some('\x08') => kprint_str("\x08 \x08"),
                Some(c) if c == '\n' || !c.is_control() => {
                    let _ = console_writer().write_char(c);
                }
                _ => {}
            },
            input::Event::Button { .. } | input::Event::Scroll { .. } => {
                let _ = writeln!(Uart, "[input] {ev:?}");
            }
            input::Event::KeyUp(_) => {}
        }
    }
}

fn kprint_str(s: &str) {
    let _ = console_writer().write_str(s);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n*** KERNEL PANIC ***\n{info}");
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
