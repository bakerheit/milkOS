//! chmod MODE FILE...: set permission bits. MODE is octal (`644`, `4755`, `1777`).
#![no_std]
#![no_main]
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (Some(mode), true) = (args.get(1), args.argv.len() > 2) else {
        rt::eprintln!("usage: chmod OCTAL-MODE FILE...");
        return 2;
    };
    let Ok(m) = u32::from_str_radix(mode, 8) else {
        rt::eprintln!("chmod: {mode}: only octal modes are supported");
        return 2;
    };
    let mut rc = 0;
    for f in &args.argv[2..] {
        if let Err(e) = rt::sys::chmod(f, m) {
            rt::eprintln!("chmod: {f}: {e}");
            rc = 1;
        }
    }
    rc
}
