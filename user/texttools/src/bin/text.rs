//! text: lines of text (it replaced cat head tail grep sort uniq wc).
//!
//!   text show FILE...          text first [N] FILE...      text last [N] FILE...
//!   text find TEXT [FILE...]   text sort [FILE...]         text unique [FILE...]
//!   text count [FILE...]       text help
//!
//! With no FILE (or `-`) each reads stdin, so `files list | text find .txt` works. `count`
//! and `unique --count` give msh a table when asked (MILK_TABLE=1, docs/shell.md). Rules
//! and help: user/textkit.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use rt::io::{Lines, Out};
use rt::sys;
use rt::table::{Table, Ty, Val};
use textkit::help::TEXT;
use textkit::lines::{self, Counts, Matcher, Opts, Runs, VERBS};
use texttools::{close, inputs, open};

rt::main!(main);

struct Ctx {
    out: Out,
    table: bool,
    rc: i32,
}

impl Ctx {
    /// Report a file that couldn't be read; the exit code becomes `rc`.
    fn err(&mut self, msg: &str, rc: i32) {
        self.out.flush();
        rt::eprintln!("text: {msg}");
        self.rc = rc;
    }

    /// Each input in turn, open, with `f(ctx, fd, name)`; errors reported with exit code `rc`.
    fn each(&mut self, files: &[&str], rc: i32, mut f: impl FnMut(&mut Ctx, i32, &str)) {
        for p in inputs(files) {
            match open(p) {
                Ok(fd) => {
                    f(self, fd, p);
                    close(fd);
                }
                Err(m) => self.err(&m, rc),
            }
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let Some(&verb) = a.first() else {
        rt::eprintln!("{TEXT}");
        return 2;
    };
    let mut c = Ctx {
        out: Out::stdout(),
        table: rt::table::requested(args),
        rc: 0,
    };
    if matches!(verb, "help" | "--help" | "-h") || a[1..].contains(&"--help") {
        c.out.write(TEXT.as_bytes());
        c.out.flush();
        return 0;
    }
    if !VERBS.contains(&verb) {
        rt::eprintln!("text: there's no text {verb} (text help lists them)");
        return 2;
    }
    let (o, rest) = match lines::parse(verb, &a[1..]) {
        Ok(v) => v,
        Err(m) => {
            rt::eprintln!("{m}");
            return 2;
        }
    };
    match verb {
        "show" => show(&mut c, &o, &rest),
        "first" => first(&mut c, &rest),
        "last" => last(&mut c, &rest),
        "find" => find(&mut c, &o, &rest),
        "sort" => sort(&mut c, &o, &rest),
        "unique" => unique(&mut c, &o, &rest),
        _ => count(&mut c, &o, &rest),
    }
    c.out.flush();
    c.rc
}

// ---- show, first, last ------------------------------------------------------------------

fn show(c: &mut Ctx, o: &Opts, files: &[&str]) {
    let mut n = 0;
    c.each(files, 1, |c, fd, _| {
        if o.numbers {
            for l in Lines::new(fd) {
                n += 1;
                let _ = writeln!(c.out, "{n:>6}  {l}");
            }
            return;
        }
        let mut buf = [0u8; 4096];
        loop {
            match sys::read(fd, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(k) => {
                    c.out.flush();
                    if sys::write_all(1, &buf[..k]).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// "==> name <==" above each file when there are several (as head and tail did).
fn heading(c: &mut Ctx, many: bool, name: &str) {
    if many {
        c.out.line(&format!("==> {name} <=="));
    }
}

fn first(c: &mut Ctx, rest: &[&str]) {
    let (n, files) = lines::how_many(rest);
    let many = files.len() > 1;
    c.each(&files, 1, |c, fd, name| {
        heading(c, many, name);
        let mut it = Lines::new(fd);
        for _ in 0..n {
            match it.next_bytes() {
                Some(l) => {
                    c.out.write(&l);
                    c.out.write(b"\n");
                }
                None => break,
            }
        }
    });
}

fn last(c: &mut Ctx, rest: &[&str]) {
    let (n, files) = lines::how_many(rest);
    let many = files.len() > 1;
    c.each(&files, 1, |c, fd, name| {
        heading(c, many, name);
        let mut keep: VecDeque<Vec<u8>> = VecDeque::new();
        let mut it = Lines::new(fd);
        while let Some(l) = it.next_bytes() {
            if n == 0 {
                continue;
            }
            if keep.len() == n {
                keep.pop_front();
            }
            keep.push_back(l);
        }
        for l in keep {
            c.out.write(&l);
            c.out.write(b"\n");
        }
    });
}

// ---- find -----------------------------------------------------------------------------------

/// Exit code: 0 something matched, 1 nothing did, 2 a file couldn't be read.
fn find(c: &mut Ctx, o: &Opts, rest: &[&str]) {
    let Some((&pat, files)) = rest.split_first() else {
        rt::eprintln!("usage: text find TEXT [FILE...]   (text help lists them all)");
        c.rc = 2;
        return;
    };
    let m = Matcher::new(pat, o.ignore_case, o.invert);
    let many = files.len() > 1;
    let mut any = false;
    c.each(files, 2, |c, fd, name| {
        let mut hits = 0;
        for (i, line) in Lines::new(fd).enumerate() {
            if !m.hit(&line) {
                continue;
            }
            hits += 1;
            any = true;
            if o.count || o.names {
                continue;
            }
            let mut s = String::new();
            if many {
                let _ = write!(s, "{name}:");
            }
            if o.numbers {
                let _ = write!(s, "{}:", i + 1);
            }
            s.push_str(&line);
            c.out.line(&s);
        }
        if o.names {
            if hits > 0 {
                c.out.line(name);
            }
        } else if o.count {
            if many {
                let _ = writeln!(c.out, "{name}:{hits}");
            } else {
                let _ = writeln!(c.out, "{hits}");
            }
        }
    });
    if c.rc == 0 && !any {
        c.rc = 1;
    }
}

// ---- sort, unique ---------------------------------------------------------------------------

fn sort(c: &mut Ctx, o: &Opts, files: &[&str]) {
    let mut all: Vec<String> = Vec::new();
    c.each(files, 2, |_, fd, _| all.extend(Lines::new(fd)));
    lines::sort(&mut all, o);
    for l in &all {
        c.out.line(l);
    }
}

fn unique(c: &mut Ctx, o: &Opts, files: &[&str]) {
    let mut runs = Runs::default();
    let mut table =
        (c.table && o.count).then(|| Table::new(&[("count", Ty::Int), ("line", Ty::Str)]));
    let mut emit = |c: &mut Ctx, (l, n): (String, usize)| {
        if o.repeated && n < 2 {
            return;
        }
        if let Some(t) = table.as_mut() {
            t.push(alloc::vec![Val::Int(n as i64), Val::Str(l)]);
        } else if o.count {
            let _ = writeln!(c.out, "{n:>7} {l}");
        } else {
            c.out.line(&l);
        }
    };
    c.each(files, 1, |c, fd, _| {
        for l in Lines::new(fd) {
            if let Some(r) = runs.push(l) {
                emit(c, r);
            }
        }
    });
    if let Some(r) = runs.finish() {
        emit(c, r);
    }
    if let Some(t) = table {
        t.write(&mut c.out);
    }
}

// ---- count ----------------------------------------------------------------------------------

fn count(c: &mut Ctx, o: &Opts, files: &[&str]) {
    let (l, w, b) = match (o.lines, o.words, o.bytes) {
        (false, false, false) => (true, true, true),
        v => v,
    };
    let mut rows: Vec<(Counts, String)> = Vec::new();
    c.each(files, 1, |_, fd, name| {
        let mut t = Counts::default();
        let mut buf = [0u8; 4096];
        while let Ok(n) = sys::read(fd, &mut buf) {
            if n == 0 {
                break;
            }
            t.feed(&buf[..n]);
        }
        rows.push((t, String::from(name)));
    });
    let picked = [l, w, b].iter().filter(|x| **x).count();
    if picked == 1 && files.len() <= 1 && rows.len() == 1 && !c.table {
        // Just the number, for scripts: text count --lines notes.txt
        let t = rows[0].0;
        let v = if l {
            t.lines
        } else if w {
            t.words
        } else {
            t.bytes
        };
        let _ = writeln!(c.out, "{v}");
        return;
    }
    if rows.len() > 1 {
        let mut total = Counts::default();
        for (t, _) in &rows {
            total.add(t);
        }
        rows.push((total, String::from("total")));
    }
    let mut cols: Vec<(&str, Ty)> = Vec::new();
    for (on, name) in [(l, "lines"), (w, "words"), (b, "bytes")] {
        if on {
            cols.push((name, Ty::Int));
        }
    }
    let named = !files.is_empty();
    if named {
        cols.push(("file", Ty::Str));
    }
    let mut t = Table::new(&cols);
    for (n, name) in &rows {
        let mut r = Vec::new();
        for (on, v) in [(l, n.lines), (w, n.words), (b, n.bytes)] {
            if on {
                r.push(Val::Int(v as i64));
            }
        }
        if named {
            r.push(Val::str(name));
        }
        t.push(r);
    }
    if c.table {
        t.write(&mut c.out);
    } else {
        t.render(&mut c.out);
    }
}
