//! msh, the milk shell: reads lines from the console, runs built-ins or programs.
//!
//! Built-ins: echo, ps, uname, sleep, exit, help, cd, pwd, ls, cat, mkdir.
//! Anything else runs `/bin/<name>` (or a path containing '/') with fork + execve + wait4.
//! A trailing `&` runs it in the background; finished jobs are reported before the prompt.
//! Words may be quoted with "..." or '...'.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, print, println, sys};

rt::main!(main);

const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];

fn main(_args: &rt::Args) -> i32 {
    println!("msh: milk shell on the console. 'help' lists built-ins.");
    let mut line = Vec::new();
    let mut last = 0;
    loop {
        reap_background();
        let cwd = sys::getcwd().unwrap_or_else(|_| String::from("?"));
        print!("msh:{cwd}$ ");
        line.clear();
        if !read_line(&mut line) {
            println!();
            println!("msh: end of input, bye");
            return 0;
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        let mut words = split(&text);
        if words.is_empty() {
            continue;
        }
        let background = words.last().is_some_and(|w| w == "&");
        if background {
            words.pop();
            if words.is_empty() {
                continue;
            }
        }
        let argv: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
        if let Some(code) = builtin(&argv, &mut last) {
            if let Builtin::Exit(c) = code {
                return c;
            }
            continue;
        }
        last = run(&argv, background);
    }
}

/// Read one line (the console tty delivers whole lines). False on end of input.
fn read_line(out: &mut Vec<u8>) -> bool {
    let mut buf = [0u8; 256];
    loop {
        match sys::read(0, &mut buf) {
            Ok(0) => return !out.is_empty(),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.last() == Some(&b'\n') {
                    out.pop();
                    return true;
                }
            }
            Err(e) if e.0 == 11 => sys::sleep_ms(10), // EAGAIN: no blocking tty
            Err(e) => {
                eprintln!("msh: read: {e:?}");
                return false;
            }
        }
    }
}

fn split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None;
    let mut have = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                have = true;
            }
            (None, c) if c.is_whitespace() => {
                if have || !cur.is_empty() {
                    out.push(core::mem::take(&mut cur));
                    have = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if have || !cur.is_empty() {
        out.push(cur);
    }
    out
}

enum Builtin {
    Done,
    Exit(i32),
}

fn builtin(argv: &[&str], last: &mut i32) -> Option<Builtin> {
    let arg = |i: usize| argv.get(i).copied();
    match argv[0] {
        "help" => {
            println!("built-ins:");
            println!("  echo [words]     print words");
            println!("  ps               processes and kernel threads");
            println!("  uname [-a]       system name");
            println!("  sleep SECS       sleep (fractions ok: 0.5)");
            println!("  cd [DIR], pwd    change / show directory");
            println!("  ls [DIR], cat FILE..., mkdir DIR");
            println!("  exit [CODE]      leave the shell (init starts a new one)");
            println!("  help             this text");
            println!("programs in /bin run by name; add '&' to run in the background.");
            println!("try: hello, fault, fault stack 2048, ps, cat /proc/meminfo");
        }
        "echo" => println!("{}", argv[1..].join(" ")),
        "exit" => {
            return Some(Builtin::Exit(
                arg(1).and_then(|s| s.parse().ok()).unwrap_or(*last),
            ));
        }
        "uname" => match sys::uname() {
            Ok(u) if arg(1) == Some("-a") => {
                println!("{} {} {} {} {}", u[0], u[1], u[2], u[3], u[4])
            }
            Ok(u) => println!("{}", u[0]),
            Err(e) => println!("uname: {e}"),
        },
        "sleep" => match arg(1).and_then(parse_secs) {
            Some(ns) => {
                let _ = sys::nanosleep(ns);
            }
            None => println!("usage: sleep SECONDS"),
        },
        "ps" => ps(),
        "pwd" => println!("{}", sys::getcwd().unwrap_or_default()),
        "cd" => {
            if let Err(e) = sys::chdir(arg(1).unwrap_or("/home/milk")) {
                println!("cd: {}: {e}", arg(1).unwrap_or("/home/milk"));
            }
        }
        "ls" => ls(arg(1).unwrap_or(".")),
        "cat" => {
            for f in &argv[1..] {
                cat(f);
            }
        }
        "mkdir" => {
            for d in &argv[1..] {
                if let Err(e) = sys::mkdir(d, 0o755) {
                    println!("mkdir: {d}: {e}");
                }
            }
        }
        _ => return None,
    }
    *last = 0;
    Some(Builtin::Done)
}

fn parse_secs(s: &str) -> Option<u64> {
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut ns = int.parse::<u64>().ok()?.checked_mul(1_000_000_000)?;
    let mut scale = 100_000_000;
    for c in frac.chars().take(9) {
        ns += c.to_digit(10)? as u64 * scale;
        scale /= 10;
    }
    Some(ns)
}

fn ps() {
    let Ok(list) = sys::ps() else {
        println!("ps: MILK_PS failed");
        return;
    };
    println!("  PID  PPID   TID S     TIME     RSS  NAME");
    for p in list {
        let (pid, ppid, rss, name) = if p.kernel {
            ("-".into(), "-".into(), "-".into(), format!("[{}]", p.name))
        } else {
            (
                format!("{}", p.pid),
                format!("{}", p.ppid),
                format!("{}K", p.rss / 1024),
                p.name,
            )
        };
        println!(
            "{pid:>5} {ppid:>5} {:>5} {} {:>6}.{:02}s {rss:>7}  {name}",
            p.tid,
            p.state,
            p.cpu_ns / 1_000_000_000,
            p.cpu_ns / 10_000_000 % 100,
        );
    }
}

fn ls(dir: &str) {
    match sys::read_dir(dir) {
        Ok(mut ents) => {
            ents.retain(|(n, _)| n != "." && n != "..");
            ents.sort_by(|a, b| a.0.cmp(&b.0));
            let mut line = String::new();
            for (name, t) in ents {
                let suffix = match t {
                    4 => "/",
                    10 => "@",
                    2 | 6 => "%",
                    _ => "",
                };
                let item = format!("{name}{suffix}");
                if line.len() + item.len() + 2 > 78 {
                    println!("{line}");
                    line.clear();
                }
                line += &format!("{item:<16}");
            }
            if !line.is_empty() {
                println!("{}", line.trim_end());
            }
        }
        Err(e) => println!("ls: {dir}: {e}"),
    }
}

fn cat(path: &str) {
    let fd = match sys::open(path, sys::O_RDONLY | sys::O_CLOEXEC) {
        Ok(fd) => fd,
        Err(e) => {
            println!("cat: {path}: {e}");
            return;
        }
    };
    let mut buf = [0u8; 1024];
    loop {
        match sys::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = sys::write_all(1, &buf[..n]);
            }
            Err(e) => {
                println!("cat: {path}: {e}");
                break;
            }
        }
    }
    let _ = sys::close(fd);
}

fn run(argv: &[&str], background: bool) -> i32 {
    let path = if argv[0].contains('/') {
        String::from(argv[0])
    } else {
        format!("/bin/{}", argv[0])
    };
    let pid = match sys::spawn(&path, argv, ENV) {
        Ok(p) => p,
        Err(e) => {
            println!("msh: fork: {e}");
            return 1;
        }
    };
    if background {
        println!("[{pid}] {}", argv.join(" "));
        return 0;
    }
    match sys::wait4(pid as i32, 0) {
        Ok((_, st)) => {
            if st != 0 && (st >> 8) & 0xff != 127 {
                println!("msh: {}: {}", argv[0], sys::describe_status(st));
            }
            if st & 0x7f == 0 {
                (st >> 8) & 0xff
            } else {
                128 + (st & 0x7f)
            }
        }
        Err(e) => {
            println!("msh: wait: {e:?}");
            1
        }
    }
}

fn reap_background() {
    while let Ok((pid, st)) = sys::wait4(-1, sys::WNOHANG) {
        if pid == 0 {
            break;
        }
        println!("[{pid}] done ({})", sys::describe_status(st));
    }
}
