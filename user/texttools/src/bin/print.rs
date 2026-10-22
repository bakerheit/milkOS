//! print [--no-newline] [--escapes] words...   (it replaced echo; -n and -e still work)
//!
//! Words are printed as they are, with a space between and a newline after. --escapes
//! works out \n \t \e \\ and \c (user/textkit/src/escape.rs). Anything else that starts
//! with a dash is a word to print; `--` prints everything after it as it is.
#![no_std]
#![no_main]
extern crate alloc;

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let mut words = &args.argv[1..];
    if words == ["--help"] {
        let _ = rt::sys::write_all(1, textkit::help::PRINT.as_bytes());
        return 0;
    }
    let (mut no_newline, mut escapes) = (false, false);
    while let Some(w) = words.first() {
        match *w {
            "--no-newline" | "-n" => no_newline = true,
            "--escapes" | "-e" => escapes = true,
            "-ne" | "-en" => (no_newline, escapes) = (true, true),
            "--" => {
                words = &words[1..];
                break;
            }
            _ => break,
        }
        words = &words[1..];
    }
    let s = textkit::escape::line(words, escapes, no_newline);
    match rt::sys::write_all(1, s.as_bytes()) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
