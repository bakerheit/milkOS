#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::write_volatile;

mod audio;
mod audit;
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
mod klog;
mod mem;
mod memtest;
mod mmu;
mod net;
mod percpu;
mod pmm;
mod proc;
mod ramfb;
mod sched;
mod selftest;
mod sink;
mod smp;
mod splash;
mod sync;
mod timer;
mod tlb;
mod uart;
mod virtio;
mod wm;

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
        // IRQs off and the print lock held for the whole line, so threads and CPUs don't
        // interleave mid-line.
        $crate::sink::with_print_lock(|| {
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
    percpu::init_boot();
    // MMU + caches before anything takes a lock (atomics hang on Device memory).
    mem::init();
    display::init();
    kprintln!("milkOS 0.0.1 booting...");
    kprintln!("hello from the kernel");

    gic::init();
    timer::init();
    uart::init();
    virtio::init();
    sync::irq_enable();
    selftest::run();
    memtest::run();
    fs::init();
    fs::selftest::run();
    sched::init();
    smp::init();
    sched::selftest::run();
    audit::init();
    net::init();
    net::selftest::run();
    audio::init();
    proc::init();
    proc::start_init();
    fs::start_threads();

    // The desktop: compositor, window manager and shell, in its own thread. It owns the
    // screen and draws the mouse cursor from here on.
    sched::spawn_prio("wm", sched::Priority::High, wm::run);
    // kmain is now just thread 0.
    loop {
        sched::sleep_ms(1000);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n*** KERNEL PANIC ***\n{info}");
    wm::panic_screen(info);
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
