//! today: switch the desktop to the Today home (same as F4); run it again to go back.

#![no_std]
#![no_main]

rt::main!(main);

fn main(_args: &rt::Args) -> i32 {
    desk::wm("today")
}
