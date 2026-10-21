//! ps [-k]: processes from /proc (-k: kernel threads too).
#![no_std]
#![no_main]
extern crate alloc;
use alloc::format;
use core::fmt::Write;
use rt::io::{Out, flags};
rt::main!(main);
fn main(args: &rt::Args) -> i32 {
    let (f, _) = flags(&args.argv[1..]);
    let Ok(list) = rt::sys::ps(f.contains(&'k')) else {
        rt::eprintln!("ps: cannot read /proc");
        return 1;
    };
    let mut out = Out::stdout();
    if rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("pid", Ty::Int),
            ("ppid", Ty::Int),
            ("pgid", Ty::Int),
            ("tid", Ty::Int),
            ("state", Ty::Str),
            ("cpu", Ty::Int),
            ("rss", Ty::Int),
            ("name", Ty::Str),
        ]);
        for p in list {
            let k = |v: u32| {
                if p.kernel {
                    Val::Nil
                } else {
                    Val::Int(v as i64)
                }
            };
            t.push(alloc::vec![
                k(p.pid),
                k(p.ppid),
                k(p.pgid),
                Val::Int(p.tid as i64),
                Val::Str(alloc::string::String::from(p.state)),
                Val::Int((p.cpu_ns / 1_000_000) as i64),
                if p.kernel {
                    Val::Nil
                } else {
                    Val::Int((p.rss / 1024) as i64)
                },
                Val::Str(if p.kernel {
                    format!("[{}]", p.name)
                } else {
                    p.name.clone()
                }),
            ]);
        }
        t.write(&mut out);
        out.flush();
        return 0;
    }
    out.line("  PID  PPID  PGID   TID S     TIME     RSS  NAME");
    for p in list {
        let (pid, ppid, pgid, rss, name) = if p.kernel {
            (
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                format!("[{}]", p.name),
            )
        } else {
            (
                format!("{}", p.pid),
                format!("{}", p.ppid),
                format!("{}", p.pgid),
                format!("{}K", p.rss / 1024),
                p.name,
            )
        };
        let _ = writeln!(
            out,
            "{pid:>5} {ppid:>5} {pgid:>5} {:>5} {} {:>6}.{:02}s {rss:>7}  {name}",
            p.tid,
            p.state,
            p.cpu_ns / 1_000_000_000,
            p.cpu_ns / 10_000_000 % 100
        );
    }
    out.flush();
    0
}
