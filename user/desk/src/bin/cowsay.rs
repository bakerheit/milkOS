//! cowsay [text...]: the cow from the wallpaper has something to say.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::println;

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let text = if args.argv.len() > 1 {
        args.argv[1..].join(" ")
    } else {
        String::from("moo")
    };
    let n = text.chars().count();
    println!(" {}", "_".repeat(n + 2));
    println!("< {text} >");
    println!(" {}", "-".repeat(n + 2));
    println!("        \\   ^__^");
    println!("         \\  (oo)\\_______");
    println!("            (__)\\       )\\/\\");
    println!("                ||----w |");
    println!("                ||     ||");
    0
}
