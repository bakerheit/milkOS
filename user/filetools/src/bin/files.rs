//! files: one command for files and folders (it replaced mv cp touch rm mkdir ls ln).
//!
//!   files create NAME            files create NAME/ (a folder, parents too)
//!   files copy SRC... DST        files move SRC... DST       files rename OLD NEW
//!   files delete [--now] PATH... (to the Trash, or for good)
//!   files info PATH...           files list [--long] [--all] [PATH...]
//!   files link TARGET NAME       files help
//!
//! A slash at the end of a path means "a folder": on `create` it decides what is made;
//! everywhere else what is on disk decides and a slash on a file is an error. Nothing is
//! overwritten unless `--replace` says so. `files list` gives msh a table when asked
//! (MILK_TABLE=1, docs/shell.md). Rules: user/filekit; file system work: filetools.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use coreutils::{fmt_perm, fmt_time};
use filekit::path::{self, Dest, Kind, check_name, check_slash, landing, split_slash};
use filekit::trash::INFO_DIR;
use filekit::words::{count, grouped, human, say, say_read, size};
use filetools::{CopyOpts, Reflink};
use rt::io::Out;
use rt::sys::{self, Stat};
use rt::table::{Table, Ty, Val};

rt::main!(main);

const HELP: &str = "\
usage: files COMMAND ...    files and folders (deleted things go to the Trash)

  create NAME             make an empty file; NAME/ (a slash) makes a folder, parents too
  copy SRC... DST         copy files, and folders with everything in them
  move SRC... DST         move or rename, folders too, across disks too
  rename OLD NEW          rename in place (NEW is a name, not a path)
  delete PATH...          move to the Trash (trash list shows it, trash restore undoes it)
  delete --now PATH...    delete for good, skipping the Trash
  info PATH...            kind, size, dates, owner and permissions
  list [PATH...]          what's in a folder: --long (-l) details, --all (-a) hidden too
  link TARGET NAME        make NAME a link to TARGET
  help                    this list

When DST is a folder, copy and move put things inside it. Nothing is overwritten unless
you add --replace. copy --reflink shares blocks (milkfs). A slash at the end of a path
means a folder: files delete notes.txt/ on a file is an error, not a guess.
";

#[derive(Default)]
struct Opts {
    now: bool,
    long: bool,
    all: bool,
    human: bool,
    replace: bool,
    reflink: Option<Reflink>,
}

struct Ctx {
    home: String,
    cwd: String,
    out: Out,
    table: bool,
    rc: i32,
}

impl Ctx {
    fn err(&mut self, msg: &str) {
        self.out.flush();
        rt::eprintln!("files: {msg}");
        self.rc = 1;
    }

    fn usage(&mut self, what: &str) {
        self.out.flush();
        rt::eprintln!("usage: files {what}   (files help lists them all)");
        self.rc = 2;
    }

    fn abs(&self, p: &str) -> String {
        path::resolve(&self.cwd, p)
    }
}

/// Options for `cmd` out of `args`; the rest are paths. Err: the bad option.
fn parse(cmd: &str, args: &[&'static str]) -> Result<(Opts, Vec<&'static str>), String> {
    let mut o = Opts::default();
    let mut rest = Vec::new();
    let mut done = false;
    let ok = |names: &[&str], a: &str| names.contains(&cmd) || a == "--help";
    for &a in args {
        if done || a == "-" || !a.starts_with('-') {
            rest.push(a);
            continue;
        }
        match a {
            "--" => done = true,
            "--now" if ok(&["delete"], a) => o.now = true,
            "--long" if ok(&["list"], a) => o.long = true,
            "--all" if ok(&["list"], a) => o.all = true,
            "--human" if ok(&["list"], a) => o.human = true,
            "--replace" if ok(&["copy", "move"], a) => o.replace = true,
            "--reflink" | "--reflink=always" if ok(&["copy"], a) => {
                o.reflink = Some(Reflink::Always)
            }
            "--reflink=auto" if ok(&["copy"], a) => o.reflink = Some(Reflink::Auto),
            "--reflink=never" if ok(&["copy"], a) => o.reflink = Some(Reflink::Never),
            _ if cmd == "list" && !a.starts_with("--") => {
                for c in a[1..].chars() {
                    match c {
                        'l' => o.long = true,
                        'a' => o.all = true,
                        'h' => o.human = true,
                        _ => return Err(format!("-{c}")),
                    }
                }
            }
            _ => return Err(String::from(a)),
        }
    }
    Ok((o, rest))
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let Some(&cmd) = a.first() else {
        rt::eprintln!("{HELP}");
        return 2;
    };
    let mut c = Ctx {
        home: filetools::home(args),
        cwd: filetools::cwd(),
        out: Out::stdout(),
        table: rt::table::requested(args),
        rc: 0,
    };
    if matches!(cmd, "help" | "--help" | "-h") || a[1..].contains(&"--help") {
        c.out.write(HELP.as_bytes());
        c.out.flush();
        return 0;
    }
    let (o, paths) = match parse(cmd, &a[1..]) {
        Ok(v) => v,
        Err(bad) => {
            let known = matches!(
                cmd,
                "create" | "copy" | "move" | "rename" | "delete" | "info" | "list" | "link"
            );
            if known {
                rt::eprintln!("files {cmd}: there's no option {bad} (try files help)");
            } else {
                rt::eprintln!("files: there's no files {cmd} (try files help)");
            }
            return 2;
        }
    };
    match cmd {
        "create" => create(&mut c, &paths),
        "copy" => transfer(&mut c, &o, &paths, false),
        "move" => transfer(&mut c, &o, &paths, true),
        "rename" => rename(&mut c, &paths),
        "delete" => delete(&mut c, &o, &paths),
        "info" => info(&mut c, &paths),
        "list" => list(&mut c, &o, &paths),
        "link" => link(&mut c, &paths),
        _ => {
            rt::eprintln!("files: there's no files {cmd} (try files help)");
            return 2;
        }
    }
    c.out.flush();
    c.rc
}

/// lstat, with the message for a path that isn't there.
fn look(c: &mut Ctx, p: &str) -> Option<Stat> {
    match sys::stat_path(p, false) {
        Ok(st) => Some(st),
        Err(e) => {
            c.err(&say_read(p, e.0));
            None
        }
    }
}

/// Is `abs` (lstat `st`) where a disk is mounted?
fn mount_point(abs: &str, st: &Stat) -> bool {
    abs != "/" && filetools::volume_root(abs, st.dev) == abs
}

// ---- create ---------------------------------------------------------------------------------

fn create(c: &mut Ctx, paths: &[&str]) {
    if paths.is_empty() {
        return c.usage("create NAME...   (NAME/ with a slash makes a folder)");
    }
    for &arg in paths {
        let (p, slash) = split_slash(arg);
        if p.is_empty() {
            c.err("an empty name can't be created");
            continue;
        }
        if slash {
            if let Err(m) = filetools::make_folders(p) {
                c.err(&m);
            }
            continue;
        }
        let up = path::parent(p);
        if up != "."
            && !filetools::is_folder(&up)
            && let Err(m) = filetools::make_folders(&up)
        {
            c.err(&m);
            continue;
        }
        match sys::open_mode(
            p,
            sys::O_WRONLY | sys::O_CREAT | sys::O_EXCL | sys::O_CLOEXEC,
            0o644,
        ) {
            Ok(fd) => {
                let _ = sys::close(fd);
            }
            Err(e) if e.0 == 17 => {
                // Already there: a file gets its time set to now (as touch did).
                if filetools::is_folder(p) {
                    c.err(&format!("{p} is a folder, not a file"));
                } else if let Err(e) = sys::touch_now(p) {
                    c.err(&say(p, e.0));
                }
            }
            Err(e) => c.err(&say(p, e.0)),
        }
    }
}

// ---- copy / move / rename -------------------------------------------------------------------

fn transfer(c: &mut Ctx, o: &Opts, paths: &[&str], moving: bool) {
    let verb = if moving { "move" } else { "copy" };
    if paths.len() < 2 {
        return c.usage(&format!(
            "{verb} SRC... DST   (DST a folder: into it){}",
            if moving { "" } else { "  [--reflink[=auto]]" }
        ));
    }
    let (srcs, dst) = paths.split_at(paths.len() - 1);
    let (dp, dslash) = split_slash(dst[0]);
    let dest = match sys::stat_path(dp, true) {
        Ok(s) if s.is_dir() => Dest::Folder,
        Ok(_) => Dest::NotFolder,
        Err(_) => Dest::Missing,
    };
    if srcs.len() > 1 && dest != Dest::Folder {
        return c.err(&format!(
            "{dp} isn't a folder (to {verb} several things, the last one has to be a folder)"
        ));
    }
    let opts = CopyOpts {
        reflink: o.reflink.unwrap_or(Reflink::Never),
        replace: o.replace,
    };
    for &src in srcs {
        let (sp, sslash) = split_slash(src);
        let Some(st) = look(c, sp) else {
            continue;
        };
        if let Err(m) = check_slash(sp, sslash, filetools::kind(&st)) {
            c.err(&m);
            continue;
        }
        let target = match landing(sp, dp, dslash, dest) {
            Ok(t) => t,
            Err(m) => {
                c.err(&m);
                continue;
            }
        };
        let (sa, ta) = (c.abs(sp), c.abs(&target));
        if sa == ta {
            c.err(&format!("{sp} and {target} are the same"));
            continue;
        }
        if st.is_dir() && path::is_within(&ta, &sa) {
            c.err(&format!("can't {verb} {sp} into itself"));
            continue;
        }
        if let Ok(t) = sys::stat_path(&target, false) {
            if !o.replace {
                c.err(&format!(
                    "{target} already exists (add --replace to overwrite it)"
                ));
                continue;
            }
            if t.is_dir() || st.is_dir() {
                c.err(&format!(
                    "{target} already exists, and --replace only replaces a file with a file"
                ));
                continue;
            }
        }
        if !moving {
            if let Err(m) = filetools::copy_tree(sp, &target, &opts) {
                c.err(&m);
            }
            continue;
        }
        if mount_point(&sa, &st) {
            c.err(&format!("{sp} is a mounted disk, so it can't be moved"));
            continue;
        }
        let r = if o.replace {
            sys::rename(sp, &target)
        } else {
            sys::rename_noreplace(sp, &target)
        };
        match r {
            Ok(()) => {}
            // Another disk: copy, then delete the original (only once the copy is whole).
            Err(e) if e.0 == 18 => {
                let plain = CopyOpts {
                    reflink: Reflink::Never,
                    replace: o.replace,
                };
                match filetools::copy_tree(sp, &target, &plain) {
                    Ok(()) => {
                        if let Err(m) = filetools::remove_tree(sp) {
                            c.err(&format!(
                                "copied {sp} to {target}, but couldn't delete the original: {m}"
                            ));
                        }
                    }
                    Err(m) => {
                        if !o.replace {
                            let _ = filetools::remove_tree(&target);
                        }
                        c.err(&format!("{m} (nothing was moved)"));
                    }
                }
            }
            Err(e) if e.0 == 17 => c.err(&format!("{target} already exists")),
            Err(e) => c.err(&say(sp, e.0)),
        }
    }
}

fn rename(c: &mut Ctx, paths: &[&str]) {
    if paths.len() != 2 {
        return c.usage("rename OLD NEW   (NEW is a name; to move it elsewhere: files move)");
    }
    let (op, oslash) = split_slash(paths[0]);
    let new = paths[1].strip_suffix('/').unwrap_or(paths[1]);
    let Some(st) = look(c, op) else {
        return;
    };
    if let Err(m) = check_slash(op, oslash, filetools::kind(&st)) {
        return c.err(&m);
    }
    if let Err(m) = check_name(new) {
        return c.err(&m);
    }
    if mount_point(&c.abs(op), &st) {
        return c.err(&format!("{op} is a mounted disk, so it can't be renamed"));
    }
    let up = path::parent(op);
    let target = if up == "." {
        String::from(new)
    } else {
        path::join(&up, new)
    };
    if filetools::exists(&target) {
        return c.err(&format!("{new} already exists there"));
    }
    if let Err(e) = sys::rename_noreplace(op, &target) {
        c.err(&say(op, e.0));
    }
}

// ---- delete ---------------------------------------------------------------------------------

fn delete(c: &mut Ctx, o: &Opts, paths: &[&str]) {
    if paths.is_empty() {
        return c.usage("delete [--now] PATH...   (to the Trash; --now: for good)");
    }
    for &arg in paths {
        let (p, slash) = split_slash(arg);
        let last = path::basename(p);
        if p.is_empty() || last == "." || last == ".." {
            c.err(&format!("won't delete {arg}: say which folder by its name"));
            continue;
        }
        let Some(st) = look(c, p) else {
            continue;
        };
        if let Err(m) = check_slash(p, slash, filetools::kind(&st)) {
            c.err(&m);
            continue;
        }
        let abs = c.abs(p);
        if abs == "/" {
            c.err("won't delete / (that's everything)");
            continue;
        }
        if abs == c.home {
            c.err(&format!("won't delete {p}: it's your home folder"));
            continue;
        }
        if filetools::is_a_trash(&abs, &c.home) {
            c.err(&format!(
                "{p} is the Trash itself; to empty it: trash empty"
            ));
            continue;
        }
        if mount_point(&abs, &st) {
            c.err(&format!("{p} is a mounted disk, so it can't be deleted"));
            continue;
        }
        // Already in a Trash: the only way on is out for good.
        if let Some((dir, name, top)) = filetools::trash_of(&abs, &c.home) {
            if name == INFO_DIR {
                c.err(&format!(
                    "{p} is where the Trash keeps its records; trash empty clears it"
                ));
                continue;
            }
            match filetools::remove_tree(p) {
                Ok(()) => {
                    if top {
                        let _ = sys::unlink(&path::join(&path::join(&dir, INFO_DIR), &name));
                    }
                    let _ = writeln!(c.out, "{p} was in the Trash: deleted for good");
                }
                Err(m) => c.err(&m),
            }
            continue;
        }
        let r = if o.now {
            filetools::remove_tree(p)
        } else {
            filetools::move_to_trash(&abs, p, &st, &c.home).map(|_| ())
        };
        if let Err(m) = r {
            c.err(&m);
        }
    }
}

// ---- info -----------------------------------------------------------------------------------

/// The `st` to show for an argument: a slash on a link to a folder means the folder.
fn look_through(c: &mut Ctx, p: &str, slash: bool) -> Option<Stat> {
    let mut st = look(c, p)?;
    if slash
        && st.is_link()
        && let Ok(t) = sys::stat_path(p, true)
    {
        st = t;
    }
    if let Err(m) = check_slash(p, slash, filetools::kind(&st)) {
        c.err(&m);
        return None;
    }
    Some(st)
}

/// Who may do what, in words.
fn perm_words(st: &Stat) -> String {
    let dir = st.is_dir();
    let can = |bits: u32| -> String {
        let mut v: Vec<&str> = Vec::new();
        if bits & 4 != 0 {
            v.push(if dir { "see inside" } else { "read" });
        }
        if bits & 2 != 0 {
            v.push(if dir { "add and remove" } else { "change" });
        }
        if bits & 1 != 0 {
            v.push(if dir { "open" } else { "run" });
        }
        if v.is_empty() {
            String::from("nothing")
        } else {
            v.join(", ")
        }
    };
    let m = st.mode;
    let mut s = format!(
        "{}: {}; group {}: {}; everyone else: {}",
        rt::users::user_name(st.uid),
        can(m >> 6 & 7),
        rt::users::group_name(st.gid),
        can(m >> 3 & 7),
        can(m & 7)
    );
    if m & 0o4000 != 0 {
        s.push_str("; runs as its owner (set-user-ID)");
    }
    if m & 0o2000 != 0 {
        s.push_str("; runs with its group (set-group-ID)");
    }
    if m & 0o1000 != 0 {
        s.push_str("; only owners may delete things in it (sticky)");
    }
    s
}

fn info(c: &mut Ctx, paths: &[&str]) {
    if paths.is_empty() {
        return c.usage("info PATH...");
    }
    for (k, &arg) in paths.iter().enumerate() {
        let (p, slash) = split_slash(arg);
        let Some(st) = look_through(c, p, slash) else {
            continue;
        };
        let kind = filetools::kind(&st);
        let abs = c.abs(p);
        let mut rows: Vec<(&str, String)> = Vec::new();
        rows.push(("kind", String::from(kind.word())));
        match kind {
            Kind::Folder => {
                let n = sys::read_dir(p).map_or(0, |l| {
                    l.iter().filter(|(n, _)| n != "." && n != "..").count()
                });
                rows.push(("contains", count(n, "item", "items")));
            }
            Kind::Link => {
                let target = sys::readlink(p).unwrap_or_default();
                let what = match sys::stat_path(p, true) {
                    Ok(t) => format!("a {}", filetools::kind(&t).word()),
                    Err(_) => String::from("missing"),
                };
                rows.push(("links to", format!("{target} ({what})")));
            }
            _ => rows.push((
                "size",
                if st.size < 1024 {
                    size(st.size)
                } else {
                    format!("{} ({} bytes)", size(st.size), grouped(st.size))
                },
            )),
        }
        rows.push(("where", abs.clone()));
        let root = filetools::volume_root(&abs, st.dev);
        let fs = sys::statfs(&root).map_or("?", |f| f.type_name());
        let ro = sys::statfs(&root).is_ok_and(|f| f.read_only());
        rows.push((
            "on",
            format!("{root} ({fs}{})", if ro { ", read-only" } else { "" }),
        ));
        rows.push(("modified", filekit::date::when(st.mtime)));
        rows.push(("changed", filekit::date::when(st.ctime)));
        rows.push(("last read", filekit::date::when(st.atime)));
        rows.push((
            "owner",
            format!(
                "{}, group {}",
                rt::users::user_name(st.uid),
                rt::users::group_name(st.gid)
            ),
        ));
        rows.push((
            "permissions",
            format!(
                "{} ({:o}): {}",
                fmt_perm(st.mode),
                st.mode & 0o7777,
                perm_words(&st)
            ),
        ));
        if k > 0 {
            c.out.line("");
        }
        c.out.line(p);
        for (key, v) in rows {
            let _ = writeln!(c.out, "  {key:<12} {v}");
        }
    }
}

// ---- list -----------------------------------------------------------------------------------

fn kind_short(st: &Stat) -> &'static str {
    match st.type_char() {
        'd' => "folder",
        'l' => "link",
        'c' | 'b' => "device",
        'p' => "pipe",
        's' => "socket",
        _ => "file",
    }
}

fn long_line(c: &mut Ctx, o: &Opts, full: &str, name: &str, st: &Stat) {
    let size = if st.is_dir() || matches!(st.type_char(), 'c' | 'b') {
        String::from("-")
    } else if o.human {
        human(st.size)
    } else {
        format!("{}", st.size)
    };
    let mut l = format!(
        "{:<6}  {}  {:<5} {:<5} {:>8}  {}  {name}",
        kind_short(st),
        fmt_perm(st.mode),
        rt::users::user_name(st.uid),
        rt::users::group_name(st.gid),
        size,
        fmt_time(st.mtime),
    );
    if st.is_link()
        && let Ok(t) = sys::readlink(full)
    {
        let _ = write!(l, " -> {t}");
    }
    c.out.line(&l);
}

fn columns(c: &mut Ctx, names: &[String]) {
    if names.is_empty() {
        return;
    }
    if !sys::isatty(1) {
        for n in names {
            c.out.line(n);
        }
        return;
    }
    let width = sys::term_cols().max(10);
    let w = names.iter().map(|n| n.chars().count()).max().unwrap_or(1) + 2;
    let ncols = (width / w).max(1);
    let nrows = names.len().div_ceil(ncols);
    for r in 0..nrows {
        let mut line = String::new();
        for k in 0..ncols {
            if let Some(n) = names.get(k * nrows + r) {
                let _ = write!(line, "{n:<w$}");
            }
        }
        c.out.line(line.trim_end());
    }
}

/// Entries shown: folders first, then by name (ignoring case).
fn sort_entries(v: &mut [(String, Stat)]) {
    v.sort_by(|a, b| {
        (!a.1.is_dir(), a.0.to_ascii_lowercase(), &a.0).cmp(&(
            !b.1.is_dir(),
            b.0.to_ascii_lowercase(),
            &b.0,
        ))
    });
}

fn show(
    c: &mut Ctx,
    o: &Opts,
    table: &mut Option<Table>,
    dir: &str,
    many: bool,
    v: &[(String, Stat)],
) {
    if let Some(t) = table.as_mut() {
        for (n, st) in v {
            let name = if dir.is_empty() || !many {
                n.clone()
            } else {
                path::join(dir, n)
            };
            t.push(alloc::vec![
                Val::Str(name),
                Val::str(kind_short(st)),
                Val::Int(st.size as i64),
                Val::Str(fmt_perm(st.mode)),
                Val::Str(rt::users::user_name(st.uid)),
                Val::Str(fmt_time(st.mtime)),
            ]);
        }
        return;
    }
    if o.long {
        for (n, st) in v {
            let full = if dir.is_empty() {
                n.clone()
            } else {
                path::join(dir, n)
            };
            long_line(c, o, &full, n, st);
        }
        return;
    }
    let names: Vec<String> = v
        .iter()
        .map(|(n, st)| {
            if st.is_dir() {
                format!("{n}/")
            } else {
                n.clone()
            }
        })
        .collect();
    columns(c, &names);
}

fn list(c: &mut Ctx, o: &Opts, paths: &[&str]) {
    let paths: Vec<&str> = if paths.is_empty() {
        alloc::vec!["."]
    } else {
        paths.to_vec()
    };
    let mut table = c.table.then(|| {
        Table::new(&[
            ("name", Ty::Str),
            ("kind", Ty::Str),
            ("size", Ty::Size),
            ("mode", Ty::Str),
            ("owner", Ty::Str),
            ("modified", Ty::Str),
        ])
    });
    let mut files: Vec<(String, Stat)> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for &arg in &paths {
        let (p, slash) = split_slash(arg);
        let Some(st) = look_through(c, p, slash) else {
            continue;
        };
        // A link to a folder, named on the command line, is listed as the folder.
        let folder = st.is_dir() || (st.is_link() && filetools::is_folder(p));
        if folder {
            dirs.push(String::from(p));
        } else {
            files.push((String::from(p), st));
        }
    }
    let many = files.len() + dirs.len() > 1;
    sort_entries(&mut files);
    show(c, o, &mut table, "", many, &files);
    for (i, d) in dirs.iter().enumerate() {
        let list = match sys::read_dir(d) {
            Ok(l) => l,
            Err(e) => {
                c.err(&say_read(d, e.0));
                continue;
            }
        };
        let mut v: Vec<(String, Stat)> = Vec::new();
        let abs = path::resolve(&c.cwd, d);
        for (n, _) in list {
            // Dot files, and /dev /proc /tmp in the root (filekit::path::hidden).
            if n == "." || n == ".." || (!o.all && path::hidden(&abs, &n)) {
                continue;
            }
            let st = sys::stat_path(&path::join(d, &n), false).unwrap_or_default();
            v.push((n, st));
        }
        sort_entries(&mut v);
        if many && table.is_none() {
            if i > 0 || !files.is_empty() {
                c.out.line("");
            }
            let _ = writeln!(c.out, "{d}:");
        }
        show(c, o, &mut table, d, many, &v);
    }
    if let Some(t) = table {
        t.write(&mut c.out);
    }
}

// ---- link -----------------------------------------------------------------------------------

fn link(c: &mut Ctx, paths: &[&str]) {
    if paths.len() != 2 {
        return c.usage("link TARGET NAME   (NAME becomes a link to TARGET)");
    }
    let target = paths[0];
    let (name, slash) = split_slash(paths[1]);
    if slash {
        return c.err(&format!("{name}: a link's name can't end with a slash"));
    }
    if filetools::exists(name) {
        return c.err(&format!("{name} already exists"));
    }
    if let Err(e) = sys::symlink(target, name) {
        return c.err(&say(name, e.0));
    }
    // Relative targets are relative to the link's folder: say if it points at nothing.
    if sys::stat_path(name, true).is_err() {
        c.out.flush();
        rt::eprintln!(
            "files: note: {target} doesn't exist (yet), so {name} points at nothing for now"
        );
    }
}
