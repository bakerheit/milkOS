//! clear: ANSI clear screen + home.
#![no_std]
#![no_main]
rt::main!(main);
fn main(_: &rt::Args) -> i32 {
    let _ = rt::sys::write_all(1, b"\x1b[2J\x1b[H");
    0
}
