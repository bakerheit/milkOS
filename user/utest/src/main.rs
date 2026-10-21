//! Kernel feature tests from user space, one per subcommand. Prints PASS/FAIL lines.
//!   utest pipe    pipe + fork + dup2 + exec: read another program's stdout
//!   utest all     everything

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{println, sys};

rt::main!(main);

fn check(name: &str, ok: bool, detail: &str) -> bool {
    println!(
        "utest: {} {name}: {detail}",
        if ok { "PASS" } else { "FAIL" }
    );
    ok
}

/// Run `argv` with stdout on a pipe, return everything it printed and its wait status.
fn capture(argv: &[&str]) -> (Vec<u8>, i32) {
    let (r, w) = sys::pipe().expect("pipe");
    let pid = match sys::fork().expect("fork") {
        0 => {
            sys::dup2(w, 1).expect("dup2");
            let e = sys::execve(argv[0], argv, &[]);
            rt::eprintln!("exec {}: {e}", argv[0]);
            sys::exit(127)
        }
        p => p,
    };
    let _ = sys::close(w);
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        match sys::read(r, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                println!("utest: read: {e:?}");
                break;
            }
        }
    }
    let _ = sys::close(r);
    let (_, st) = sys::wait4(pid as i32, 0).unwrap_or((0, -1));
    (out, st)
}

fn t_pipe() -> bool {
    let (out, st) = capture(&["/bin/hello", "piped"]);
    let text = String::from_utf8_lossy(&out);
    let lines = text.lines().count();
    let mut ok = check(
        "pipe+dup2+exec",
        st == 0 && text.starts_with("hello, world!") && text.contains("\"piped\""),
        &alloc::format!(
            "read {} bytes / {lines} lines from hello's stdout, status {st}",
            out.len()
        ),
    );
    // Big transfer through the pipe between two processes of ours.
    let (r, w) = sys::pipe().unwrap();
    const N: usize = 200_000;
    let pid = match sys::fork().unwrap() {
        0 => {
            let _ = sys::close(r);
            let chunk: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
            for _ in 0..N / 5000 {
                sys::write_all(w, &chunk).unwrap();
            }
            sys::exit(0)
        }
        p => p,
    };
    let _ = sys::close(w);
    let mut total = 0;
    let mut good = true;
    let mut buf = [0u8; 3000];
    loop {
        let n = sys::read(r, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        good &= buf[..n]
            .iter()
            .enumerate()
            .all(|(k, &b)| b == ((total + k) % 5000 % 251) as u8);
        total += n;
    }
    let _ = sys::close(r);
    let _ = sys::wait4(pid as i32, 0);
    ok &= check(
        "pipe 200 KB",
        total == N && good,
        &alloc::format!("{total} bytes, data {}", if good { "ok" } else { "BAD" }),
    );
    // Reader gone: write fails with EPIPE (32).
    let (r, w) = sys::pipe().unwrap();
    let _ = sys::close(r);
    let e = sys::write(w, b"x");
    let _ = sys::close(w);
    ok &= check("EPIPE", e == Err(sys::Errno(32)), &alloc::format!("{e:?}"));
    ok
}

fn main(args: &rt::Args) -> i32 {
    let what = args.get(1).unwrap_or("all");
    let mut ok = true;
    if what == "pipe" || what == "all" {
        ok &= t_pipe();
    }
    println!("utest: {}", if ok { "ALL PASS" } else { "SOME FAILED" });
    if ok { 0 } else { 1 }
}
