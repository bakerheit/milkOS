//! whoami: the effective user's name.
#![no_std]
#![no_main]
rt::main!(main);
fn main(_args: &rt::Args) -> i32 {
    rt::println!("{}", rt::users::user_name(rt::sys::geteuid()));
    0
}
