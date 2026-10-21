//! ls [-l] [-a] [-1] [-F] [-h] [-d] [PATH...]
//! Columns fit the terminal width (TIOCGWINSZ on stdout, 80 if unknown); one name per line
//! when stdout is not a terminal, like GNU ls.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use coreutils::{fmt_perm, fmt_time, human, join};
use rt::io::{Out, flags};
use rt::sys::{self, Stat};
rt::main!(main);

struct Opts {
    long: bool,
    all: bool,
    one: bool,
    classify: bool,
    human: bool,
}

fn suffix(st: &Stat) -> &'static str {
    match st.type_char() {
        'd' => "/",
        'l' => "@",
        'p' => "|",
        _ if st.mode & 0o111 != 0 => "*",
        _ => "",
    }
}

fn long_line(out: &mut Out, o: &Opts, path: &str, name: &str, st: &Stat) {
    let size = if matches!(st.type_char(), 'c' | 'b') {
        format!("{}, {}", (st.rdev >> 8) & 0xfff, st.rdev & 0xff)
    } else if o.human {
        human(st.size)
    } else {
        format!("{}", st.size)
    };
    let mut l = format!(
        "{}{} {:>3} {:<5} {:<5} {:>8} {} {}",
        st.type_char(),
        fmt_perm(st.mode),
        st.nlink,
        rt::users::user_name(st.uid),
        rt::users::group_name(st.gid),
        size,
        fmt_time(st.mtime),
        name
    );
    if st.is_link() {
        if let Ok(t) = sys::readlink(path) {
            let _ = write!(l, " -> {t}");
        }
    } else if o.classify {
        l.push_str(suffix(st));
    }
    out.line(&l);
}

fn columns(out: &mut Out, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let width = sys::term_cols().max(10);
    let w = names.iter().map(|n| n.chars().count()).max().unwrap_or(1) + 2;
    let ncols = (width / w).max(1);
    let nrows = names.len().div_ceil(ncols);
    for r in 0..nrows {
        let mut line = String::new();
        for c in 0..ncols {
            if let Some(n) = names.get(c * nrows + r) {
                let _ = write!(line, "{n:<w$}");
            }
        }
        out.line(line.trim_end());
    }
}

fn type_name(st: &Stat) -> &'static str {
    match st.type_char() {
        'd' => "dir",
        'l' => "link",
        'c' => "char",
        'b' => "block",
        'p' => "fifo",
        's' => "socket",
        _ => "file",
    }
}

/// Where entries go: printed, or (msh asked for a table) collected into one.
struct Sink<'a> {
    out: &'a mut Out,
    table: Option<rt::table::Table>,
    many: bool,
}

fn list(sink: &mut Sink, o: &Opts, path: &str, entries: Vec<(String, Stat)>, tty: bool) {
    let many = sink.many;
    if let Some(t) = sink.table.as_mut() {
        use rt::table::Val;
        for (n, st) in entries {
            t.push(alloc::vec![
                Val::Str(if path.is_empty() || !many {
                    n
                } else {
                    join(path, &n)
                }),
                Val::str(type_name(&st)),
                Val::Int(st.size as i64),
                Val::Str(format!("{}{}", st.type_char(), fmt_perm(st.mode))),
                Val::Str(fmt_time(st.mtime)),
            ]);
        }
        return;
    }
    let out = &mut *sink.out;
    if o.long {
        if !path.is_empty() {
            // A directory listing (not the plain-file operands).
            let blocks: u64 = entries.iter().map(|e| e.1.size.div_ceil(1024)).sum();
            let _ = writeln!(out, "total {blocks}");
        }
        for (n, st) in &entries {
            long_line(out, o, &join(path, n), n, st);
        }
        return;
    }
    let names: Vec<String> = entries
        .iter()
        .map(|(n, st)| {
            if o.classify {
                format!("{n}{}", suffix(st))
            } else {
                n.clone()
            }
        })
        .collect();
    if o.one || !tty {
        for n in names {
            out.line(&n);
        }
    } else {
        columns(out, &names);
    }
}

fn main(args: &rt::Args) -> i32 {
    let (f, mut paths) = flags(&args.argv[1..]);
    let o = Opts {
        long: f.contains(&'l'),
        all: f.contains(&'a'),
        one: f.contains(&'1'),
        classify: f.contains(&'F'),
        human: f.contains(&'h'),
    };
    let dir_itself = f.contains(&'d');
    if paths.is_empty() {
        paths.push(".");
    }
    let tty = sys::isatty(1);
    let mut out = Out::stdout();
    let table = rt::table::requested(args).then(|| {
        rt::table::Table::new(&[
            ("name", rt::table::Ty::Str),
            ("type", rt::table::Ty::Str),
            ("size", rt::table::Ty::Size),
            ("mode", rt::table::Ty::Str),
            ("modified", rt::table::Ty::Str),
        ])
    });
    let mut rc = 0;
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for p in &paths {
        match sys::stat_path(p, false) {
            Ok(st) if st.is_dir() && !dir_itself => dirs.push(*p),
            Ok(st) => files.push((String::from(*p), st)),
            Err(e) => {
                rt::eprintln!("ls: {p}: {e}");
                rc = 2;
            }
        }
    }
    let many = files.len() + dirs.len() > 1;
    let mut sink = Sink {
        out: &mut out,
        table,
        many,
    };
    list(&mut sink, &o, "", files, tty);
    for (i, d) in dirs.iter().enumerate() {
        if many && sink.table.is_none() {
            if i > 0 || !out_empty(&paths, &dirs) {
                sink.out.line("");
            }
            let _ = writeln!(sink.out, "{d}:");
        }
        let mut ents: Vec<(String, Stat)> = Vec::new();
        match sys::read_dir(d) {
            Ok(list_) => {
                for (n, _) in list_ {
                    if (n == "." || n == "..") && !o.all {
                        continue;
                    }
                    if n.starts_with('.') && !o.all {
                        continue;
                    }
                    let st = sys::stat_path(&join(d, &n), false).unwrap_or_default();
                    ents.push((n, st));
                }
            }
            Err(e) => {
                rt::eprintln!("ls: {d}: {e}");
                rc = 2;
                continue;
            }
        }
        ents.sort_by(|a, b| a.0.cmp(&b.0));
        list(&mut sink, &o, d, ents, tty);
    }
    if let Some(t) = sink.table.take() {
        t.write(sink.out);
    }
    out.flush();
    rc
}

/// True if no plain files were printed before the first directory.
fn out_empty(paths: &[&str], dirs: &[&str]) -> bool {
    paths.len() == dirs.len()
}
