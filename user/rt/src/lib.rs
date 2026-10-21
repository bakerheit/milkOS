//! Tiny runtime for milkOS user programs: `_start`, Linux aarch64 syscall wrappers,
//! `print!`/`println!`, a brk-backed bump allocator and a panic handler.
//!
//! A program looks like:
//! ```ignore
//! #![no_std]
//! #![no_main]
//! rt::main!(main);
//! fn main(args: &rt::Args) -> i32 { rt::println!("hi"); 0 }
//! ```

#![no_std]

extern crate alloc;

pub mod alloc_brk;
pub mod io;
pub mod net;
pub mod sha256;
pub mod sys;
pub mod table;
pub mod thread;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};

pub use sys::Errno;

/// Command line and environment.
pub struct Args {
    pub argv: Vec<&'static str>,
    pub envp: Vec<&'static str>,
}

impl Args {
    pub fn get(&self, i: usize) -> Option<&'static str> {
        self.argv.get(i).copied()
    }
    pub fn env(&self, key: &str) -> Option<&'static str> {
        self.envp
            .iter()
            .find_map(|e| e.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
    }
}

unsafe extern "Rust" {
    fn __rt_main(args: &Args) -> i32;
}

/// Declare the program's entry point: `rt::main!(main)` with `fn main(&rt::Args) -> i32`.
#[macro_export]
macro_rules! main {
    ($f:path) => {
        #[unsafe(no_mangle)]
        fn __rt_main(args: &$crate::Args) -> i32 {
            $f(args)
        }
    };
}

/// Process entry. The kernel leaves sp at argc (Linux layout: argc, argv..., 0, envp..., 0,
/// auxv...).
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!("mov x0, sp", "mov x29, xzr", "mov x30, xzr", "bl {start}", "brk #1", start = sym rt_start);
}

unsafe fn cstr(p: *const u8) -> &'static str {
    let mut n = 0;
    // SAFETY: the kernel put NUL-terminated strings on our stack.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(p, n)).unwrap_or("?")
    }
}

extern "C" fn rt_start(sp: *const u64) -> ! {
    // SAFETY: Linux initial stack layout, built by the kernel's ELF loader.
    let args = unsafe {
        let argc = *sp as usize;
        let argv_p = sp.add(1) as *const *const u8;
        let argv = (0..argc).map(|i| cstr(*argv_p.add(i))).collect();
        let mut envp = Vec::new();
        let mut e = argv_p.add(argc + 1);
        while !(*e).is_null() {
            envp.push(cstr(*e));
            e = e.add(1);
        }
        Args { argv, envp }
    };
    // SAFETY: provided by the program through `main!`.
    let code = unsafe { __rt_main(&args) };
    sys::exit(code)
}

// ---- printing ------------------------------------------------------------------------

/// Format into a buffer, then one write(2), so lines from different processes don't mix.
pub fn print_to(fd: i32, args: fmt::Arguments) {
    let mut s = String::new();
    let _ = s.write_fmt(args);
    let _ = sys::write_all(fd, s.as_bytes());
}

#[macro_export]
macro_rules! print {
    ($($a:tt)*) => { $crate::print_to(1, format_args!($($a)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print_to(1, format_args!("\n")) };
    ($($a:tt)*) => { $crate::print_to(1, format_args!("{}\n", format_args!($($a)*))) };
}

#[macro_export]
macro_rules! eprintln {
    ($($a:tt)*) => { $crate::print_to(2, format_args!("{}\n", format_args!($($a)*))) };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    print_to(2, format_args!("panic: {info}\n"));
    sys::exit(101)
}
