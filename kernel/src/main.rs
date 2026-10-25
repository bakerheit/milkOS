#![no_std]
#![no_main]

extern crate alloc;

use core::fmt::{self, Write};
use core::panic::PanicInfo;

mod art;
mod audio;
mod audit;
mod crashscreen;
mod crypto;
mod cursor;
mod display;
mod dma;
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
mod journal;
mod klog;
mod mem;
mod memtest;
mod mmu;
mod net;
mod pci;
mod percpu;
mod platform;
mod pmm;
mod poll;
mod proc;
mod pty;
mod ramfb;
#[cfg(feature = "raspi")]
mod rpi;
mod sched;
mod selftest;
mod serror;
mod sink;
mod smp;
mod splash;
mod sync;
mod sysfont;
mod sysfont_data;
mod sysstat;
mod timeout;
mod timer;
mod tlb;
mod uart;
mod udrv;
mod usb;
mod virtio;
mod wm;

// boot.S (the entry point) is included by mmu.rs, which owns the constants it uses.

/// milkOS version, from `<repo>/VERSION` (kernel/build.rs).
pub const VERSION: &str = env!("MILKOS_VERSION");

/// The console UART (uart.rs: the board's PL011, plus the Pi's mini UART when the firmware
/// enabled it).
struct Uart;

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                uart::putc(b'\r');
            }
            uart::putc(b);
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
            // The console, and the journal (unit "kernel").
            let _ = core::fmt::Write::write_fmt(&mut $crate::journal::Tee, format_args!($($arg)*));
            let _ = core::fmt::Write::write_str(&mut $crate::journal::Tee, "\n");
        });
        // Outside the lock: the Pi's LCD panel (if there is one) gets what the line changed.
        $crate::sink::after_print();
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
    // The Pi's UART needs setting up before the first character (a no-op on virt).
    uart::early_init();
    // Before anything else of ours touched a device: an SError the firmware left pending is taken (and named) here.
    serror::checkpoint("kmain-entry");
    #[cfg(feature = "raspi")]
    rpi::led::on();
    percpu::init_boot();
    // MMU + caches before anything takes a lock (atomics hang on Device memory).
    mem::init();
    // `milk.serror=` (the command line is readable now), then the first checkpoint.
    serror::init();
    serror::checkpoint("mem-init");
    #[cfg(feature = "raspi")]
    rpi::led::start_codes();
    serror::checkpoint("led-codes");
    // The 3.5-inch SPI panel on the GPIO header, if the command line has one: up before the
    // display layer so the boot log can reach it.
    #[cfg(feature = "raspi")]
    rpi::lcd::init();
    serror::checkpoint("lcd-init");
    display::init();
    serror::checkpoint("display-init");
    kprintln!("milkOS {VERSION} booting...");
    kprintln!("hello from the kernel");
    platform::check_dtb();
    #[cfg(feature = "raspi")]
    {
        rpi::diag::boot_report();
        rpi::diag::dtb_report();
    }
    serror::checkpoint("boot-reports");

    gic::init();
    serror::checkpoint("gic-init");
    timer::init();
    serror::checkpoint("timer-init");
    uart::init();
    serror::checkpoint("uart-init");
    serror::measure();
    #[cfg(feature = "raspi")]
    rpi::led::stage(rpikit::ledcode::Stage::Interrupts, true);
    #[cfg(feature = "raspi")]
    rpi::init();
    serror::checkpoint("rpi-init-done");
    udrv::init_policy();
    selftest::crash_if_asked("early");
    // Which of the slow boot self-tests run (opt/milkos/selftest; by default none).
    selftest::read_option();
    virtio::init();
    serror::checkpoint("virtio-init");
    crypto::init();
    serror::checkpoint("crypto-init");
    sync::irq_enable();
    serror::checkpoint("irq-enable");
    #[cfg(feature = "raspi")]
    rpi::lcd::irqs_live();
    serror::checkpoint("lcd-irqs-live");
    if selftest::wants(selftest::TIMER) {
        selftest::run();
    }
    memtest::run();
    serror::checkpoint("memtest");
    fs::init();
    serror::checkpoint("fs-init");
    // Crash-test boots (opt/milkos/test) always; the fs self-test only if asked for.
    fs::selftest::run();
    serror::checkpoint("sched-init-entry");
    sched::init();
    serror::checkpoint("sched-init");
    smp::init();
    serror::checkpoint("smp-init");
    #[cfg(feature = "raspi")]
    rpi::led::stage(rpikit::ledcode::Stage::Scheduler, true);
    if selftest::wants(selftest::SCHED) {
        sched::selftest::run();
    }
    audit::init();
    journal::init();
    sysstat::init();
    serror::checkpoint("audit-journal-sysstat");
    net::init();
    serror::checkpoint("net-init");
    net::tls::init();
    serror::checkpoint("net-tls-init");
    if selftest::wants(selftest::NET) {
        net::selftest::run();
    } else {
        // The network is up when the shell is, self-test or not.
        net::selftest::wait_up();
    }
    net::tls::selftest();
    serror::checkpoint("net-up");
    audio::init();
    serror::checkpoint("audio-init");
    udrv::init_files();
    serror::checkpoint("udrv-init-files");
    proc::init();
    serror::checkpoint("proc-init");
    proc::start_init();
    serror::checkpoint("proc-start-init");
    #[cfg(feature = "raspi")]
    rpi::led::stage(rpikit::ledcode::Stage::Userspace, true);
    // Saves the boot log into the log file on the card, every second or so (rpi/logfile.rs).
    #[cfg(feature = "raspi")]
    rpi::logfile::start_thread();
    serror::checkpoint("logfile-thread");
    fs::start_threads();
    serror::checkpoint("fs-threads");
    #[cfg(feature = "raspi")]
    rpi::lcd::start();
    serror::checkpoint("lcd-start");
    #[cfg(feature = "raspi")]
    rpi::genet::start();
    serror::checkpoint("genet-start");
    usb::start();
    serror::checkpoint("usb-start");

    // The desktop: compositor, window manager and shell, in its own thread. It owns the
    // screen and draws the mouse cursor from here on.
    sched::spawn_prio("wm", sched::Priority::High, wm::run);
    // kmain is now just thread 0.
    selftest::crash_if_asked("late");
    loop {
        sched::sleep_ms(1000);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("\n*** KERNEL PANIC ***\n{info}");
    #[cfg(feature = "raspi")]
    rpi::led::note_panic(info);
    wm::panic_screen(info);
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}
