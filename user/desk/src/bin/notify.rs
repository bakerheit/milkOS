//! notify [--app NAME] [--id N] [--action LABEL[:COMMAND]]... <title> [subtitle...]
//! Show a toast on the desktop (and a card in the notification drawer). Up to two
//! --action buttons: with a command, pressing one runs it (an absolute path or a /bin
//! program and its words, or `open <what>`); without, the posting app's window gets an
//! action event. --app sets the group and icon (default: notify), --id a number sent back.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::println;

rt::main!(main);

fn usage() -> i32 {
    println!(
        "usage: notify [--app NAME] [--id N] [--action LABEL[:COMMAND]]... <title> [subtitle...]"
    );
    1
}

fn main(args: &rt::Args) -> i32 {
    let mut opts: Vec<String> = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.argv.iter().skip(1);
    while let Some(a) = it.next() {
        let key = match *a {
            "--app" => "app",
            "--id" => "id",
            "--action" => "action",
            "-h" | "--help" => return usage(),
            _ => {
                words.push(a);
                continue;
            }
        };
        let Some(v) = it.next() else {
            return usage();
        };
        if v.contains('\t') {
            return usage();
        }
        opts.push(alloc::format!("{key}={v}"));
    }
    let Some(title) = words.first() else {
        return usage();
    };
    let sub = words[1..].join(" ");
    let mut line = alloc::format!("notify\t{title}\t{sub}");
    for o in opts {
        line.push('\t');
        line.push_str(&o);
    }
    desk::wm(&line)
}
