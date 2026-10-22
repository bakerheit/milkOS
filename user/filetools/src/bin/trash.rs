//! trash: what's in the Trash, putting things back, emptying it.
//!
//!   trash [list]                   name, where from, when deleted, days left
//!   trash restore NAME|PATH...     back where it came from (refused if something is there)
//!   trash empty [--yes]            delete everything in it for good (asks first)
//!
//! `files delete` puts things in; `trashd` deletes them for good once they're older than
//! Settings > Trash says (30 days unless changed). `trash list` gives msh a table when
//! asked (MILK_TABLE=1).
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use filekit::date::when_short;
use filekit::path;
use filekit::trash::{Pick, pick};
use filekit::words::{count, size};
use filetools::Item;
use rt::io::Out;
use rt::sys;
use rt::table::{Table, Ty, Val};

rt::main!(main);

const HELP: &str = "\
usage: trash COMMAND ...    the Trash (files delete puts things in it)

  list                    what's in it: where from, when, days left (the default)
  restore NAME|PATH...    put things back where they came from
  empty [--yes]           delete everything in it for good (asks first without --yes)
  help                    this list

Things stay in the Trash for 30 days, then they're deleted for good (Settings > Trash
changes how long, or to never). NAME is the name in the Trash (trash list); PATH is
where it was.
";

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let home = filetools::home(args);
    let mut out = Out::stdout();
    let rc = match a.first().copied().unwrap_or("list") {
        "list" | "ls" => list(&mut out, &home, rt::table::requested(args)),
        "restore" | "put-back" => restore(&mut out, &home, &a[1..]),
        "empty" => empty(&mut out, &home, &a[1..]),
        "help" | "--help" | "-h" => {
            out.write(HELP.as_bytes());
            0
        }
        other => {
            rt::eprintln!("trash: there's no trash {other} (try trash help)");
            2
        }
    };
    out.flush();
    rc
}

/// `~/...` for paths in the home folder.
fn tilde(p: &str, home: &str) -> String {
    match p.strip_prefix(home) {
        Some(rest) if home != "/" && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => String::from(p),
    }
}

fn days_left_text(left: Option<i64>) -> String {
    match left {
        None => String::from("never"),
        Some(0) => String::from("today"),
        Some(d) => format!("{d}"),
    }
}

/// A line of `trash list`: name, from, deleted, days left, size, the item.
type Row<'a> = (String, String, String, Option<i64>, u64, &'a Item);

fn list(out: &mut Out, home: &str, table: bool) -> i32 {
    let items = filetools::my_items(home);
    let keep = filetools::keep_for(home);
    let now = filetools::now();
    let rows: Vec<Row> = items
        .iter()
        .map(|it| {
            let from = it.info.as_ref().map_or_else(
                || String::from("(unknown)"),
                |i| String::from(i.path.as_str()),
            );
            let name = if it.st.is_dir() {
                format!("{}/", it.name)
            } else {
                it.name.clone()
            };
            (
                name,
                from,
                when_short(it.deleted(), now),
                keep.days_left(it.deleted(), now),
                filetools::tree_size(&it.path()),
                it,
            )
        })
        .collect();
    if table {
        let mut t = Table::new(&[
            ("name", Ty::Str),
            ("from", Ty::Str),
            ("deleted", Ty::Str),
            ("days_left", Ty::Int),
            ("size", Ty::Size),
            ("trash", Ty::Str),
        ]);
        for (_, from, del, left, size, it) in &rows {
            t.push(alloc::vec![
                Val::Str(it.name.clone()),
                Val::Str(from.clone()),
                Val::Str(del.clone()),
                left.map_or(Val::Nil, Val::Int),
                Val::Int(*size as i64),
                Val::Str(it.trash.clone()),
            ]);
        }
        t.write(out);
        return 0;
    }
    if rows.is_empty() {
        out.line("The Trash is empty.");
        return 0;
    }
    let wn = rows
        .iter()
        .map(|r| r.0.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let wf = rows
        .iter()
        .map(|r| tilde(&r.1, home).chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let wd = rows
        .iter()
        .map(|r| r.2.chars().count())
        .max()
        .unwrap_or(7)
        .max(7);
    let _ = writeln!(
        out,
        "{:<wn$}  {:<wf$}  {:<wd$}  {:>9}  {:>7}",
        "NAME", "FROM", "DELETED", "DAYS LEFT", "SIZE"
    );
    let mut total = 0;
    for (name, from, del, left, bytes, _) in &rows {
        total += bytes;
        let from = &tilde(from, home);
        let _ = writeln!(
            out,
            "{name:<wn$}  {from:<wf$}  {del:<wd$}  {:>9}  {:>7}",
            days_left_text(*left),
            size(*bytes)
        );
    }
    let how = match keep {
        filekit::trash::Keep::Forever => {
            String::from("Nothing is deleted for good on its own (Settings > Trash).")
        }
        k => format!(
            "Things are deleted for good after {} in the Trash (Settings > Trash).",
            k.label()
        ),
    };
    let _ = writeln!(
        out,
        "{}, {}. {how}",
        count(rows.len(), "item", "items"),
        size(total)
    );
    0
}

fn restore(out: &mut Out, home: &str, args: &[&str]) -> i32 {
    if args.is_empty() {
        rt::eprintln!("usage: trash restore NAME|PATH...   (trash list shows the names)");
        return 2;
    }
    let cwd = filetools::cwd();
    let mut rc = 0;
    for &arg in args {
        let items = filetools::my_items(home);
        let keyed: Vec<(String, Option<filekit::trash::Info>)> = items
            .iter()
            .map(|it| (it.name.clone(), it.info.clone()))
            .collect();
        let want = arg.strip_suffix('/').unwrap_or(arg);
        match pick(want, &path::resolve(&cwd, want), &keyed) {
            Pick::One(i) => match filetools::restore(&items[i]) {
                Ok(to) => {
                    let _ = writeln!(out, "put {} back: {to}", items[i].name);
                }
                Err(m) => {
                    out.flush();
                    rt::eprintln!("trash: {m}");
                    rc = 1;
                }
            },
            Pick::Nothing => {
                out.flush();
                rt::eprintln!("trash: {want} isn't in the Trash (trash list shows what is)");
                rc = 1;
            }
            Pick::Several(v) => {
                out.flush();
                let names: Vec<String> = v
                    .iter()
                    .map(|&i| {
                        let it = &items[i];
                        let from = it.info.as_ref().map_or("?", |f| f.path.as_str());
                        format!("\"{}\" (from {from})", it.name)
                    })
                    .collect();
                rt::eprintln!(
                    "trash: there are {} called {want}: say which by its name in the Trash: {}",
                    v.len(),
                    names.join(", ")
                );
                rc = 1;
            }
        }
    }
    rc
}

fn empty(out: &mut Out, home: &str, args: &[&str]) -> i32 {
    let yes = args.iter().any(|a| matches!(*a, "--yes" | "-y"));
    if let Some(bad) = args.iter().find(|a| !matches!(**a, "--yes" | "-y")) {
        rt::eprintln!("trash empty: there's no option {bad} (only --yes)");
        return 2;
    }
    let items = filetools::my_items(home);
    if items.is_empty() {
        out.line("The Trash is already empty.");
        return 0;
    }
    let what = count(items.len(), "item", "items");
    if !yes {
        out.flush();
        rt::print!("Delete {what} in the Trash for good? This can't be undone. [y/N] ");
        let mut line = alloc::vec::Vec::new();
        let mut b = [0u8; 1];
        while let Ok(1) = sys::read(0, &mut b) {
            if b[0] == b'\n' {
                break;
            }
            line.push(b[0]);
        }
        let ans = String::from_utf8_lossy(&line);
        if !matches!(ans.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
            out.line("Nothing was deleted.");
            return 1;
        }
    }
    let mut failed = 0;
    for it in &items {
        if let Err(m) = filetools::delete_item(it) {
            out.flush();
            rt::eprintln!("trash: {m}");
            failed += 1;
        }
    }
    if failed == 0 {
        let _ = writeln!(out, "Deleted {what} for good. The Trash is empty.");
        0
    } else {
        let _ = writeln!(
            out,
            "Deleted {} for good; {} couldn't be deleted (above).",
            count(items.len() - failed, "item", "items"),
            failed
        );
        1
    }
}
