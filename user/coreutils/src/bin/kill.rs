//! kill [-SIGNAL | -s SIGNAL] PID...    (negative PID: process group)
//! kill -l                              list signals
#![no_std]
#![no_main]
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut a = &args.argv[1..];
    let mut sig = sys::SIGTERM;
    match a.first() {
        Some(&"-l") => {
            for s in 1..32 {
                if sys::signal_name(s) != "?" {
                    rt::println!("{s:>2}) SIG{}", sys::signal_name(s));
                }
            }
            return 0;
        }
        Some(&"-s") => {
            sig = a.get(1).and_then(|s| sys::signal_number(s)).unwrap_or(-1);
            a = &a[2.min(a.len())..];
        }
        Some(s)
            if s.starts_with('-') && s.len() > 1 && s[1..].parse::<i32>().is_err()
                || (s.starts_with('-') && a.len() > 1) =>
        {
            sig = sys::signal_number(&s[1..]).unwrap_or(-1);
            a = &a[1..];
        }
        _ => {}
    }
    if a.first() == Some(&"--") {
        a = &a[1..];
    }
    if sig < 0 || a.is_empty() {
        rt::eprintln!("usage: kill [-SIGNAL] PID...   (kill -l lists signals)");
        return 1;
    }
    let mut rc = 0;
    for p in a {
        match p.parse::<i32>() {
            Ok(pid) => {
                if let Err(e) = sys::kill(pid, sig) {
                    rt::eprintln!(
                        "kill: {pid}: {}",
                        if e.0 == 3 { "no such process" } else { e.msg() }
                    );
                    rc = 1;
                }
            }
            Err(_) => {
                rt::eprintln!("kill: {p}: not a pid");
                rc = 1;
            }
        }
    }
    rc
}
