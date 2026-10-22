//! sandbox [OPTIONS] CMD [ARGS...]   run CMD with a reduced capability set.
//!
//!   --fs PATH:r | PATH:rw   grant read / read-write under PATH (repeatable; the first
//!                           --fs switches filesystem access from "all" to "only these")
//!   --net none|local|all    network: none, loopback/link-local only, or unrestricted
//!   --no-audio --no-clipboard --no-windows --no-spawn   drop that capability
//!
//! Capabilities only ever shrink: a sandbox inside a sandbox can't regain anything. Every
//! denied syscall is written to the audit log (`audit --event sandbox`).
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use rt::sys;
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let mut spec = String::new();
    let mut i = 1;
    let a = &args.argv;
    while i < a.len() {
        match a[i] {
            "--fs" => {
                i += 1;
                let Some(g) = a.get(i) else {
                    rt::eprintln!("sandbox: --fs needs PATH:r|rw");
                    return 2;
                };
                let g = if g.contains(':') {
                    String::from(*g)
                } else {
                    alloc::format!("{g}:r")
                };
                spec.push_str(&alloc::format!("fs={g} "));
            }
            "--net" => {
                i += 1;
                let Some(n) = a.get(i) else {
                    rt::eprintln!("sandbox: --net needs none|local|all");
                    return 2;
                };
                spec.push_str(&alloc::format!("net={n} "));
            }
            "--no-fs" => spec.push_str("fs-none "),
            "--no-audio" => spec.push_str("audio=0 "),
            "--no-clipboard" => spec.push_str("clipboard=0 "),
            "--no-windows" => spec.push_str("windows=0 "),
            "--no-spawn" => spec.push_str("spawn=0 "),
            "--" => {
                i += 1;
                break;
            }
            s if s.starts_with('-') => {
                rt::eprintln!("sandbox: unknown option {s}");
                return 2;
            }
            _ => break,
        }
        i += 1;
    }
    if i >= a.len() {
        rt::eprintln!("usage: sandbox [--fs P:rw] [--net none|local|all] [--no-audio|...] CMD...");
        return 2;
    }
    let argv: Vec<&str> = a[i..].to_vec();
    let path = if argv[0].contains('/') {
        String::from(argv[0])
    } else {
        alloc::format!("/Commands/{}", argv[0])
    };
    // If the filesystem is being restricted, the command's own binary must stay readable
    // or it couldn't be exec'd. (A no-op when fs is left unrestricted.)
    if spec.contains("fs=") || spec.contains("fs-none") {
        spec.push_str(&alloc::format!("fs={path}:r "));
    }
    if let Err(e) = sys::sandbox(spec.trim()) {
        rt::eprintln!("sandbox: bad spec: {e}");
        return 2;
    }
    let e = sys::execve(&path, &argv, &args.envp);
    rt::eprintln!("sandbox: {}: {e}", argv[0]);
    127
}
