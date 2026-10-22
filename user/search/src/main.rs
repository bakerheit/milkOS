//! search: find files by name and content through the search index (it replaced `spot`).
//!
//!   search WORDS...          paths of files whose name or text has every word (or a word
//!                            starting with it), best first
//!   search --long WORDS...   with kind, size, date and where it matched
//!   search --limit N ...     at most N results (default 50)
//!   search --status | --wait | --wait-index | --rebuild | help
//!
//! Filters: kind:md (or text, image, audio, folder), in:Documents (or in:/path),
//! modified:today|yesterday|week|month|3d|12h. Under msh's structured pipes (MILK_TABLE=1)
//! it gives a table: path, name, kind, size, modified, score, matched. Talks to
//! /System/Kernel/fs/search; parsing in user/cmdkit.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use cmdkit::args::{self, flag, valued};
use cmdkit::search as kit;
use rt::io::Out;
use rt::{eprintln, print, println, sys};

rt::main!(main);

const DIR: &str = "/System/Kernel/fs/search";

const HELP: &str = "\
usage: search WORDS...    find files by name and text (the search index)

  WORDS...                files whose name or text has every word (or a word starting with it)
  --long (-l)             with kind, size, date and where it matched
  --limit N (-n)          at most N results (default 50)
  --status                the index: its state, size and the folders it covers
  --wait                  wait until the index is ready (built, or loaded and checked)
  --wait-index            wait until there is an index to ask (a saved one answers early)
  --rebuild               index everything again from scratch, in the background
  help                    this list

Filters: kind:md (or text, image, audio, folder), in:Documents (or in:/path),
modified:today|yesterday|week|month|3d|12h. search -- help looks for the word help.
";

fn read(path: &str) -> Result<String, rt::Errno> {
    sys::read_file(path).map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn write(path: &str, s: &str) -> Result<(), rt::Errno> {
    let fd = sys::open(path, sys::O_WRONLY | sys::O_CLOEXEC)?;
    let r = sys::write(fd, s.as_bytes());
    let _ = sys::close(fd);
    r.map(|_| ())
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    if matches!(a, ["help"] | ["--help"] | ["-h"]) {
        let mut o = Out::stdout();
        o.write(HELP.as_bytes());
        o.flush();
        return 0;
    }
    let spec = [
        flag("long", Some('l')),
        valued("limit", Some('n')),
        flag("status", None),
        flag("wait", None),
        flag("wait-index", None),
        flag("rebuild", None),
        flag("help", None),
    ];
    let p = match args::parse(a, &spec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.words("search"));
            return 2;
        }
    };
    if p.has("help") {
        let mut o = Out::stdout();
        o.write(HELP.as_bytes());
        o.flush();
        return 0;
    }
    if p.has("status") {
        return match read(&format!("{DIR}/status")) {
            Ok(s) => {
                print!("{s}");
                0
            }
            Err(e) => {
                eprintln!("search: the index can't be read ({DIR}/status: {e})");
                1
            }
        };
    }
    if p.has("wait") || p.has("wait-index") {
        // --wait: until the index is ready (built, or loaded and verified); --wait-index:
        // until there is one to query (a loaded one answers while it is verified). At most
        // 10 minutes.
        let any = p.has("wait-index") && !p.has("wait");
        for _ in 0..6000 {
            let (ready, some) =
                kit::status_ready(&read(&format!("{DIR}/status")).unwrap_or_default());
            if ready || (any && some) {
                println!(
                    "search: the index is {}",
                    if ready { "ready" } else { "loaded" }
                );
                return 0;
            }
            sys::sleep_ms(100);
        }
        eprintln!("search: the index still isn't ready after 10 minutes");
        return 1;
    }
    if p.has("rebuild") {
        return match write(&format!("{DIR}/rebuild"), "1") {
            Ok(()) => {
                println!("search: rebuilding the index in the background");
                0
            }
            Err(e) => {
                eprintln!(
                    "search: couldn't start a rebuild: {}",
                    if e.0 == 1 || e.0 == 13 {
                        String::from("you don't have permission to rebuild the index")
                    } else {
                        format!("{e}")
                    }
                );
                1
            }
        };
    }
    if p.rest.is_empty() {
        eprintln!("usage: search [--long] [--limit N] WORDS...   (search help lists the options)");
        return 2;
    }
    let q = p.rest.join(" ");
    let req = match p.value("limit") {
        Some(n) => match args::number("search", "limit", n) {
            Ok(n) => format!("limit={n} {q}"),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        },
        None => q.clone(),
    };
    if let Err(e) = write(&format!("{DIR}/query"), &req) {
        eprintln!(
            "search: {}",
            match e.0 {
                11 =>
                    String::from("the index isn't ready yet (search --status says how far it is)"),
                22 => format!("{q:?} can't be searched for (a filter is misspelled?)"),
                _ => format!("{e}"),
            }
        );
        return 1;
    }
    let text = match read(&format!("{DIR}/query")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("search: the answer can't be read: {e}");
            return 1;
        }
    };
    let hits = kit::hits(&text);
    if rt::table::requested(args) {
        use rt::table::{Table, Ty, Val};
        let mut t = Table::new(&[
            ("path", Ty::Str),
            ("name", Ty::Str),
            ("kind", Ty::Str),
            ("size", Ty::Size),
            ("modified", Ty::Str),
            ("score", Ty::Int),
            ("matched", Ty::Str),
        ]);
        for h in &hits {
            t.push(alloc::vec![
                Val::str(&h.path),
                Val::str(h.name()),
                Val::str(&h.kind),
                Val::Int(h.size),
                Val::Str(kit::date(h.mtime)),
                Val::Int(h.score),
                Val::str(&h.matched),
            ]);
        }
        let mut o = Out::stdout();
        t.write(&mut o);
        o.flush();
        return 0;
    }
    for h in &hits {
        if p.has("long") {
            println!("{}", kit::long_line(h));
        } else {
            println!("{}", h.path);
        }
    }
    if hits.is_empty() {
        eprintln!("search: nothing found for {q:?}");
        return 1;
    }
    0
}
