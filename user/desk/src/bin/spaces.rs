//! spaces: switch the desktop to the Spaces overview (same as F3); run it again to go back.

#![no_std]
#![no_main]

rt::main!(main);

fn main(_args: &rt::Args) -> i32 {
    desk::wm("spaces")
}
