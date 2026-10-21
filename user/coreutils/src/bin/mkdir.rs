//! mkdir [-p] DIR...
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use rt::io::flags;
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, dirs) = flags(&args.argv[1..]);
    let parents = f.contains(&'p');
    let mut rc = 0;
    for d in dirs {
        if parents {
            let mut acc = String::new();
            for part in d.split('/') {
                if part.is_empty() {
                    if acc.is_empty() {
                        acc.push('/');
                    }
                    continue;
                }
                if !acc.is_empty() && !acc.ends_with('/') {
                    acc.push('/');
                }
                acc.push_str(part);
                match sys::mkdir(&acc, 0o755) {
                    Ok(()) => {}
                    Err(e) if e.0 == 17 => {}
                    Err(e) => {
                        rt::eprintln!("mkdir: {acc}: {e}");
                        rc = 1;
                        break;
                    }
                }
            }
        } else if let Err(e) = sys::mkdir(d, 0o755) {
            rt::eprintln!(
                "mkdir: {d}: {}",
                if e.0 == 17 { "file exists" } else { e.msg() }
            );
            rc = 1;
        }
    }
    rc
}
