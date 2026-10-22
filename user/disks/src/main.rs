//! disks: one command for disks, volumes and snapshots (it replaced df, snap and dedupe;
//! docs/filesystem.md). Everything milkfs does goes through the control files in
//! /System/Kernel/fs/milkfs; `list` is statfs(2) on each mount.
//!
//!   disks [list] [--human] [--all] [PATH...]   size and free space (was df)
//!   disks devices | info | snapshots | clones
//!   disks snapshot NAME | delete NAME | rollback NAME | changes A [B]
//!   disks clone SNAPSHOT NAME | keep NAME | discard NAME | auto [SETTINGS]
//!   disks check [--repair] | verify [start|stop|status|rate=MB/s|auto=...]
//!   disks format DEVICE [LABEL] | encrypt [DEVICE] | decrypt [DEVICE]
//!   disks lock [DEVICE|PATH] | unlock [DEVICE] | passphrase
//!   disks send SNAPSHOT | receive [FILE] | dedupe PATH | help
//!
//! `list`, `devices`, `snapshots` and `clones` give msh a table when asked (MILK_TABLE=1,
//! docs/shell.md). Secrets (passphrases, recovery keys) are asked for without echo, or read
//! from --passphrase-file, and wiped from memory after use. Parsing and words: user/cmdkit,
//! user/diskkit.

#![no_std]
#![no_main]

extern crate alloc;

mod secret;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use cmdkit::args::{self, Opt, Parsed, flag, valued};
use cmdkit::disks as words;
use rt::io::Out;
use rt::sys::{self, StatFs};
use rt::table::{Table, Ty, Val};
use rt::{eprintln, print, println};

use secret::{ask, new_passphrase, stream_passphrase, wipe};

rt::main!(main);

const HELP: &str = "\
usage: disks COMMAND ...    disks, volumes and snapshots (milkfs)

  list [PATH...]          size and free space of each disk: --human (-h), --all (-a)
  devices                 every disk: size, milkfs or not, encrypted, mounted or locked
  info                    the Milk volume: state, space, encryption, conversion progress
  snapshots               the Milk volume's snapshots
  snapshot NAME           take one (browse it at /Volumes/Milk/.snapshots/NAME)
  delete NAME             delete the snapshot NAME
  rollback NAME           make the volume what snapshot NAME captured (newer ones go away)
  changes A [B]           what changed between snapshots A and B (or A and now)
  auto [SETTINGS]         automatic snapshots: show, or set (on interval=15m keep=8 daily=14)
  clones                  the writable clones of snapshots
  clone SNAPSHOT NAME     a writable clone of SNAPSHOT, mounted at /Volumes/Milk-NAME
  keep NAME               the clone NAME becomes the volume (the old state stays, as before-NAME)
  discard NAME            throw the clone NAME away
  check [--repair]        check the volume (--repair: and fix what is safe to fix)
  verify [ACTION]         read every block and check its checksum, and wait; or start, stop,
                          status, rate=MB/s, auto=off|on|7d
  format DEVICE [LABEL]   make DEVICE a milkfs volume: --force, --encrypt
  encrypt [DEVICE]        encrypt a volume in place, in the background (--wait, --rate MB/s)
  decrypt [DEVICE]        decrypt a volume in place (--wait)
  lock [DEVICE|PATH]      lock unlocked encrypted volumes again (none named: every one)
  unlock [DEVICE]         unlock an encrypted volume and mount it
  passphrase              change the Milk volume's passphrase
  send SNAPSHOT           a snapshot as a stream: --output FILE (-o), --from OLDER (incremental)
  receive [FILE]          recreate a sent snapshot (FILE or stdin): --to PATH, --force
  dedupe PATH             make identical files under PATH share their blocks (--dry-run: report)
  help                    this list

Passphrases are asked for without echo; --passphrase-file FILE reads one instead (format,
encrypt, decrypt, send, receive). An encrypted volume's streams are encrypted: send
--plaintext doesn't, send --encrypt does from a plain one. format --encrypt and encrypt show
the recovery key once (--recovery-out FILE writes it to FILE, --no-recovery makes none).
The destructive ones need an administrator: admin ticket disks ... (or root).
";

const MILKFS: &str = "/System/Kernel/fs/milkfs";

fn ctl_path(op: &str) -> String {
    format!("{MILKFS}/{op}")
}

fn read(path: &str) -> String {
    sys::read_file(path)
        .map(|d| String::from_utf8_lossy(&d).into_owned())
        .unwrap_or_default()
}

/// Write `text` to control `op`.
fn write_ctl(op: &str, text: &str) -> Result<(), rt::Errno> {
    let fd = sys::open(&ctl_path(op), sys::O_WRONLY | sys::O_CLOEXEC)?;
    let r = sys::write(fd, text.as_bytes());
    let _ = sys::close(fd);
    r.map(|_| ())
}

/// Why control `op` just failed: the kernel's reason (/System/Kernel/fs/milkfs/error) when
/// it is about `op`, else the errno in words.
fn why(op: &str, e: rt::Errno) -> String {
    words::kernel_reason(op, &read(&ctl_path("error")), e.0).unwrap_or_else(|| words::why(e.0))
}

/// Write control `op`; on failure say so as `disks VERB[ ARG]: reason`.
fn run(verb: &str, arg: &str, op: &str, text: &str) -> Result<(), ()> {
    write_ctl(op, text).map_err(|e| {
        let arg = if arg.is_empty() {
            String::new()
        } else {
            format!(" {arg}")
        };
        eprintln!("disks {verb}{arg}: {}", why(op, e));
    })
}

/// Print a control file's text (`empty` when there is none).
fn show(op: &str, empty: &str) -> i32 {
    match sys::read_file(&ctl_path(op)) {
        Ok(d) => {
            let t = String::from_utf8_lossy(&d);
            if t.is_empty() {
                println!("{empty}");
            } else {
                print!("{t}");
            }
            0
        }
        Err(e) => {
            eprintln!("disks: {}", filekit::words::say_read(&ctl_path(op), e.0));
            1
        }
    }
}

fn abs(p: &str) -> String {
    if p.starts_with('/') {
        String::from(p)
    } else {
        let cwd = sys::getcwd().unwrap_or_else(|_| String::from("/"));
        format!("{}/{p}", cwd.trim_end_matches('/'))
    }
}

fn write_table(t: Table) -> i32 {
    let mut o = Out::stdout();
    t.write(&mut o);
    o.flush();
    0
}

/// The options each subcommand takes.
fn spec(cmd: &str) -> Option<Vec<Opt>> {
    let pf = valued("passphrase-file", None);
    Some(match cmd {
        "list" => alloc::vec![flag("human", Some('h')), flag("all", Some('a'))],
        "check" => alloc::vec![flag("repair", None)],
        "format" => alloc::vec![
            flag("force", None),
            flag("encrypt", None),
            valued("recovery-out", None),
            flag("no-recovery", None),
            pf,
        ],
        "encrypt" => alloc::vec![
            valued("recovery-out", None),
            flag("no-recovery", None),
            valued("rate", None),
            flag("wait", None),
            pf,
        ],
        "decrypt" => alloc::vec![valued("rate", None), flag("wait", None), pf],
        "send" => alloc::vec![
            valued("from", None),
            valued("output", Some('o')),
            flag("encrypt", None),
            flag("plaintext", None),
            pf,
        ],
        "receive" => alloc::vec![valued("to", None), flag("force", Some('F')), pf],
        "dedupe" => alloc::vec![flag("dry-run", Some('n'))],
        "devices" | "info" | "snapshots" | "snapshot" | "delete" | "rollback" | "changes"
        | "auto" | "clones" | "clone" | "keep" | "discard" | "verify" | "lock" | "unlock"
        | "passphrase" => Vec::new(),
        _ => return None,
    })
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let (cmd, rest) = match a.first() {
        None => ("list", &a[..0]),
        Some(&c) if c.starts_with('-') && c != "--help" => ("list", a),
        Some(&c) => (c, &a[1..]),
    };
    if matches!(cmd, "help" | "--help") || rest.contains(&"--help") {
        let mut o = Out::stdout();
        o.write(HELP.as_bytes());
        o.flush();
        return 0;
    }
    let Some(sp) = spec(cmd) else {
        eprintln!("disks: there's no disks {cmd} (try disks help)");
        return 2;
    };
    // verify's and auto's settings look like `rate=8`, never like options.
    let p = match args::parse(rest, &sp) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.words(&format!("disks {cmd}")));
            return 2;
        }
    };
    let table = rt::table::requested(args);
    let home = args.env("HOME");
    let usage = |u: &str| {
        eprintln!("usage: disks {u}   (disks help lists them all)");
        2
    };
    let r = p.rest.as_slice();
    match (cmd, r) {
        ("list", _) => list(&p, table),
        ("devices", []) => devices(table),
        ("info", []) => show("info", "(no milkfs volume)"),
        ("snapshots", []) => snapshots(table),
        ("clones", []) => clones(table),
        ("snapshot", [name]) => done(run("snapshot", name, "snapshot", name), || {
            println!("disks: took snapshot {name} (it's at /Volumes/Milk/.snapshots/{name})")
        }),
        ("delete", [name]) => done(run("delete", name, "delete", name), || {
            println!("disks: deleted snapshot {name}")
        }),
        ("rollback", [name]) => done(run("rollback", name, "rollback", name), || {
            println!("disks: the volume is back to snapshot {name}")
        }),
        ("changes", [a]) => changes(a, None),
        ("changes", [a, b]) => changes(a, Some(b)),
        ("auto", []) => show("auto", "(automatic snapshots aren't available)"),
        ("auto", set) => {
            let s = set.join(" ");
            match run("auto", &s, "auto", &s) {
                Ok(()) => show("auto", ""),
                Err(()) => 1,
            }
        }
        ("clone", [snap, name]) => clone(snap, name),
        ("keep", [name]) => done(run("keep", name, "promote", name), || {
            println!(
                "disks: clone {name} is now the volume (the old state is snapshot before-{name})"
            )
        }),
        ("discard", [name]) => done(run("discard", name, "discard", name), || {
            println!("disks: discarded clone {name}")
        }),
        ("check", []) => {
            let mode = if p.has("repair") { "repair" } else { "check" };
            match run("check", "", "check", mode) {
                Ok(()) => show("check", ""),
                Err(()) => 1,
            }
        }
        ("verify", []) => verify_wait(),
        ("verify", ["status"]) => show("scrub", ""),
        ("verify", set) => {
            let s = set.join(" ");
            match run("verify", &s, "scrub", &s) {
                Ok(()) => show("scrub", ""),
                Err(()) => 1,
            }
        }
        ("format", [dev]) => format_disk(&p, dev, "Milk", home),
        ("format", [dev, label]) => format_disk(&p, dev, label, home),
        ("encrypt", [] | [_]) => convert(&p, r.first().copied(), true, home),
        ("decrypt", [] | [_]) => convert(&p, r.first().copied(), false, home),
        ("lock", []) => lock(None),
        ("lock", [t]) => lock(Some(t)),
        ("unlock", []) => unlock(None),
        ("unlock", [d]) => unlock(Some(d)),
        ("passphrase", []) => passphrase(),
        ("send", [snap]) => send(&p, snap),
        ("receive", []) => receive(&p, None),
        ("receive", [f]) => receive(&p, Some(f)),
        ("dedupe", [path]) => dedupe(path, p.has("dry-run")),
        ("snapshot" | "delete" | "rollback", _) => usage(&format!("{cmd} NAME")),
        ("keep" | "discard", _) => usage(&format!("{cmd} NAME   (a clone's name)")),
        ("changes", _) => usage("changes SNAPSHOT [SNAPSHOT|live]"),
        ("clone", _) => usage("clone SNAPSHOT NAME"),
        ("format", _) => usage("format DEVICE [LABEL] [--force] [--encrypt]"),
        ("encrypt" | "decrypt", _) => usage(&format!("{cmd} [DEVICE] [--wait]")),
        ("lock", _) => usage("lock [DEVICE|PATH]"),
        ("unlock", _) => usage("unlock [DEVICE]"),
        ("send", _) => usage("send SNAPSHOT [--output FILE] [--from OLDER]"),
        ("receive", _) => usage("receive [FILE] [--to PATH] [--force]"),
        ("dedupe", _) => usage("dedupe [--dry-run] PATH"),
        _ => usage(cmd),
    }
}

fn done(r: Result<(), ()>, ok: impl FnOnce()) -> i32 {
    match r {
        Ok(()) => {
            ok();
            0
        }
        Err(()) => 1,
    }
}

// ---- list (statfs) --------------------------------------------------------------------------

struct Row {
    mount: String,
    st: StatFs,
}

impl Row {
    fn size(&self) -> u64 {
        self.st.blocks * self.st.bsize
    }
    fn free(&self) -> u64 {
        self.st.bfree * self.st.bsize
    }
    fn avail(&self) -> u64 {
        self.st.bavail * self.st.bsize
    }
    fn used(&self) -> u64 {
        self.size() - self.free().min(self.size())
    }
    /// Like GNU df: used / (used + available), rounded up; None without storage.
    fn use_pct(&self) -> Option<u64> {
        let (u, a) = (self.used(), self.avail());
        (u + a > 0).then(|| (u * 100).div_ceil(u + a))
    }
}

/// `disks list [--human] [--all] [PATH...]`. Without PATHs: every mount in /proc/mounts,
/// skipping the ones with no storage of their own (proc, sysfs, devfs) and repeats of the
/// same file system (bind mounts like /Users/milk); --all shows them all. Sizes in 1K
/// blocks, --human readable.
fn list(p: &Parsed, table: bool) -> i32 {
    let (human, all) = (p.has("human"), p.has("all"));
    let mut rows = Vec::new();
    let mut status = 0;
    if p.rest.is_empty() {
        let mut seen: Vec<(u64, u64)> = Vec::new();
        for line in read("/proc/mounts").lines() {
            let Some(m) = line.split(' ').nth(1) else {
                continue;
            };
            let Ok(st) = sys::statfs(m) else { continue };
            let id = (st.fs_type, st.fsid);
            if !all && (st.blocks == 0 || seen.contains(&id)) {
                continue;
            }
            seen.push(id);
            rows.push(Row {
                mount: String::from(m),
                st,
            });
        }
    } else {
        for &path in &p.rest {
            // fstatfs on the opened path (statfs if it can't be opened).
            let st = match sys::open(path, sys::O_RDONLY | sys::O_CLOEXEC) {
                Ok(fd) => {
                    let r = sys::fstatfs(fd);
                    let _ = sys::close(fd);
                    r
                }
                Err(_) => sys::statfs(path),
            };
            match st {
                Ok(st) => rows.push(Row {
                    mount: String::from(path),
                    st,
                }),
                Err(e) => {
                    eprintln!("disks list: {}", filekit::words::say_read(path, e.0));
                    status = 1;
                }
            }
        }
    }
    if table {
        let mut t = Table::new(&[
            ("mount", Ty::Str),
            ("type", Ty::Str),
            ("size", Ty::Size),
            ("used", Ty::Size),
            ("avail", Ty::Size),
            ("use_pct", Ty::Int),
            ("bsize", Ty::Int),
            ("blocks", Ty::Int),
            ("bfree", Ty::Int),
            ("bavail", Ty::Int),
            ("files", Ty::Int),
            ("ffree", Ty::Int),
            ("ro", Ty::Str),
        ]);
        for r in &rows {
            t.push(alloc::vec![
                Val::str(&r.mount),
                Val::str(r.st.type_name()),
                Val::Int(r.size() as i64),
                Val::Int(r.used() as i64),
                Val::Int(r.avail() as i64),
                r.use_pct().map_or(Val::Nil, |p| Val::Int(p as i64)),
                Val::Int(r.st.bsize as i64),
                Val::Int(r.st.blocks as i64),
                Val::Int(r.st.bfree as i64),
                Val::Int(r.st.bavail as i64),
                Val::Int(r.st.files as i64),
                Val::Int(r.st.ffree as i64),
                Val::str(if r.st.read_only() { "ro" } else { "rw" }),
            ]);
        }
        write_table(t);
        return status;
    }
    let shown = |v: u64| -> String {
        if human {
            filekit::words::human(v)
        } else {
            format!("{}", v / 1024)
        }
    };
    println!(
        "{:<8} {:>10} {:>10} {:>10} {:>5}  Mounted on",
        "Type",
        if human { "Size" } else { "1K-blocks" },
        "Used",
        "Avail",
        "Use%"
    );
    for r in &rows {
        println!(
            "{:<8} {:>10} {:>10} {:>10} {:>5}  {}{}",
            r.st.type_name(),
            shown(r.size()),
            shown(r.used()),
            shown(r.avail()),
            r.use_pct().map_or(String::from("-"), |p| format!("{p}%")),
            r.mount,
            if r.st.read_only() { " (ro)" } else { "" }
        );
    }
    status
}

// ---- devices, snapshots, clones (tables from diskkit's parsers) -----------------------------

fn devices(table: bool) -> i32 {
    if !table {
        return show("disks", "(no disks)");
    }
    use diskkit::disks::{Content, State};
    let mut t = Table::new(&[
        ("device", Ty::Str),
        ("size", Ty::Size),
        ("ro", Ty::Str),
        ("content", Ty::Str),
        ("encrypted", Ty::Str),
        ("state", Ty::Str),
        ("mount", Ty::Str),
        ("label", Ty::Str),
    ]);
    for d in diskkit::disks::parse(&read(&ctl_path("disks"))) {
        let (content, enc, state, mount, label) = match &d.content {
            Content::Other => ("other", Val::Nil, Val::Nil, Val::Nil, Val::Nil),
            Content::Milkfs {
                encrypted,
                state,
                label,
            } => {
                let (s, m) = match state {
                    State::Mounted(m) => ("mounted", Val::str(m)),
                    State::Locked => ("locked", Val::Nil),
                    State::Unmounted => ("unmounted", Val::Nil),
                };
                (
                    "milkfs",
                    Val::str(if *encrypted { "yes" } else { "no" }),
                    Val::str(s),
                    m,
                    Val::str(label),
                )
            }
        };
        t.push(alloc::vec![
            Val::str(&d.name),
            Val::Int(d.bytes as i64),
            Val::str(if d.read_only { "ro" } else { "rw" }),
            Val::str(content),
            enc,
            state,
            mount,
            label,
        ]);
    }
    write_table(t)
}

fn snapshots(table: bool) -> i32 {
    if !table {
        return show("snapshots", "(no snapshots)");
    }
    let mut t = Table::new(&[
        ("name", Ty::Str),
        ("generation", Ty::Int),
        ("created", Ty::Str),
        ("kind", Ty::Str),
    ]);
    for s in diskkit::snaps::parse_snapshots(&read(&ctl_path("snapshots"))) {
        use diskkit::snaps::SnapKind;
        t.push(alloc::vec![
            Val::str(&s.name),
            Val::Int(s.generation as i64),
            Val::Str(s.when()),
            Val::str(match s.kind {
                SnapKind::Auto => "automatic",
                SnapKind::Before => "before a keep",
                SnapKind::Manual => "taken",
            }),
        ]);
    }
    write_table(t)
}

fn clones(table: bool) -> i32 {
    if !table {
        return show("clones", "(no clones)");
    }
    let mut t = Table::new(&[("name", Ty::Str), ("snapshot", Ty::Str), ("mount", Ty::Str)]);
    for c in diskkit::snaps::parse_clones(&read(&ctl_path("clones"))) {
        t.push(alloc::vec![
            Val::str(&c.name),
            Val::str(&c.origin),
            c.mount.as_deref().map_or(Val::Nil, Val::str),
        ]);
    }
    write_table(t)
}

fn clone(snap: &str, name: &str) -> i32 {
    if run(
        "clone",
        &format!("{snap} {name}"),
        "clone",
        &diskkit::snaps::clone_request(snap, name),
    )
    .is_err()
    {
        return 1;
    }
    let at = diskkit::snaps::parse_clones(&read(&ctl_path("clones")))
        .into_iter()
        .find(|c| c.name == name)
        .and_then(|c| c.mount);
    match at {
        Some(m) => println!("disks: made clone {name} of snapshot {snap}, at {m}"),
        None => println!("disks: made clone {name} of snapshot {snap}"),
    }
    0
}

fn changes(a: &str, b: Option<&str>) -> i32 {
    match run(
        "changes",
        a,
        "diff",
        &format!("{a} {}", b.unwrap_or("live")),
    ) {
        Ok(()) => show("diff", "(no changes)"),
        Err(()) => 1,
    }
}

// ---- verify (the scrub) ---------------------------------------------------------------------

/// `disks verify`: start a scrub and follow /System/Kernel/fs/milkfs/scrub until it is done,
/// printing progress every 25%. Exit 3 if it found bad blocks.
fn verify_wait() -> i32 {
    let ctl = ctl_path("scrub");
    let before = words::scrub_run(&read(&ctl));
    if run("verify", "", "scrub", "start").is_err() {
        return 1;
    }
    let mut shown = u64::MAX;
    for _ in 0..360_000 {
        sys::sleep_ms(100);
        let t = read(&ctl);
        let first = t.lines().next().unwrap_or("");
        if words::scrub_run(first) <= before {
            continue; // not picked up yet
        }
        if first.contains("] running") {
            let pct = words::scrub_percent(first).unwrap_or(0);
            if shown == u64::MAX || pct / 25 != shown / 25 {
                eprintln!("{first}");
                shown = pct;
            }
            continue;
        }
        print!("{t}");
        return if words::scrub_found_errors(&t) { 3 } else { 0 };
    }
    eprintln!("disks verify: timed out waiting for it to finish");
    1
}

// ---- format, encrypt, decrypt ---------------------------------------------------------------

/// Show a recovery key the kernel wrote to `path` (and remove the file), or say where it is.
fn recovery_key(rec: &Option<String>, rec_out: Option<&str>, none: bool) {
    match (rec, rec_out) {
        (Some(path), None) => {
            let mut k = sys::read_file(path).unwrap_or_default();
            let _ = sys::unlink(path);
            println!(
                "disks: recovery key: {}",
                core::str::from_utf8(&k).unwrap_or("?").trim_end()
            );
            k.fill(0);
            println!("disks: write it down and keep it apart from the disk: it is shown only now");
        }
        (Some(path), Some(_)) => println!("disks: recovery key written to {path}"),
        _ if none => println!("disks: no recovery key (only the passphrase opens it)"),
        _ => {}
    }
}

/// `disks format DEVICE [LABEL] [--force] [--encrypt [--recovery-out FILE | --no-recovery]
/// [--passphrase-file F]]`.
fn format_disk(p: &Parsed, dev: &str, label: &str, home: Option<&str>) -> i32 {
    let encrypt = p.has("encrypt");
    let rec_out = p.value("recovery-out");
    if !encrypt && (rec_out.is_some() || p.has("no-recovery") || p.has("passphrase-file")) {
        eprintln!(
            "disks format: --recovery-out, --no-recovery and --passphrase-file go with --encrypt"
        );
        return 2;
    }
    let mut req = format!("{dev} {label}");
    if p.has("force") {
        req.push_str(" force");
    }
    if !encrypt {
        return done(run("format", dev, "mkfs", &req), || {
            println!("disks: {dev} formatted (milkfs, mounted at the next boot)")
        });
    }
    // The recovery key goes to a file the kernel creates (0600, owned like its folder);
    // without --recovery-out, a temporary one in the home folder, shown and removed.
    let tmp = format!(
        "{}/.milkfs-recovery-{}",
        home.unwrap_or("/tmp"),
        sys::getpid()
    );
    let rec = (!p.has("no-recovery")).then(|| rec_out.map(abs).unwrap_or_else(|| tmp.clone()));
    req.push_str(" encrypt");
    if let Some(r) = &rec {
        req.push_str(&format!(" recovery={r}"));
    }
    let secret = match p.value("passphrase-file") {
        Some(f) => stream_passphrase(Some(f), false),
        None => new_passphrase("Passphrase for the new volume (8 characters or more): "),
    };
    let Some(mut pass) = secret else {
        return 1;
    };
    let mut t = format!("{req}\n{pass}");
    let r = run("format", dev, "mkfs", &t);
    wipe(&mut t);
    wipe(&mut pass);
    if r.is_err() {
        return 1;
    }
    println!(
        "disks: {dev} formatted as an encrypted milkfs volume (locked: `disks unlock {dev}` mounts it)"
    );
    recovery_key(&rec, rec_out, true);
    0
}

/// Follow /System/Kernel/fs/milkfs/encrypt until no volume is being converted.
fn convert_wait() -> i32 {
    let mut shown = u64::MAX;
    loop {
        let t = read(&ctl_path("encrypt"));
        let Some(l) = t.lines().find(|l| l.contains(": converting: ")) else {
            for l in t.lines().filter(|l| !l.starts_with("rate")) {
                println!("disks: {l}");
            }
            return if t.contains("stopped") { 1 } else { 0 };
        };
        let pct = words::convert_percent(l).unwrap_or(0);
        if shown == u64::MAX || pct / 25 != shown / 25 {
            eprintln!("disks: {l}");
            shown = pct;
        }
        sys::sleep_ms(500);
    }
}

/// `disks encrypt [DEVICE] [--recovery-out FILE | --no-recovery] [--passphrase-file F]
/// [--rate MB] [--wait]` and `disks decrypt [DEVICE] [--passphrase-file F] [--wait]`.
fn convert(p: &Parsed, dev: Option<&str>, encrypt: bool, home: Option<&str>) -> i32 {
    let verb = if encrypt { "encrypt" } else { "decrypt" };
    let rec_out = p.value("recovery-out");
    let mut req = String::new();
    if let Some(d) = dev {
        req.push_str(&format!("dev={d} "));
    }
    let tmp = format!(
        "{}/.milkfs-recovery-{}",
        home.unwrap_or("/tmp"),
        sys::getpid()
    );
    let rec =
        (encrypt && !p.has("no-recovery")).then(|| rec_out.map(abs).unwrap_or_else(|| tmp.clone()));
    if let Some(r) = &rec {
        req.push_str(&format!("recovery={r} "));
    }
    if let Some(r) = p.value("rate")
        && run(
            verb,
            &format!("--rate {r}"),
            "encrypt",
            &format!("rate={r}"),
        )
        .is_err()
    {
        return 1;
    }
    let secret = match p.value("passphrase-file") {
        Some(f) => stream_passphrase(Some(f), false),
        None if encrypt => new_passphrase("New passphrase for the volume (8 characters or more): "),
        None => ask("Passphrase (or recovery key): "),
    };
    let Some(mut pass) = secret else {
        return 1;
    };
    let mut t = format!("{}\n{pass}", req.trim_end());
    let r = run(verb, dev.unwrap_or(""), verb, &t);
    wipe(&mut t);
    wipe(&mut pass);
    if r.is_err() {
        return 1;
    }
    println!(
        "disks: {} in place started (the volume stays usable; progress: disks info)",
        if encrypt { "encrypting" } else { "decrypting" }
    );
    recovery_key(&rec, rec_out, false);
    if p.has("wait") { convert_wait() } else { 0 }
}

// ---- lock, unlock, passphrase ---------------------------------------------------------------

fn unlock(dev: Option<&str>) -> i32 {
    let locked = read(&ctl_path("unlock"));
    let target = locked
        .lines()
        .find(|l| dev.is_none_or(|d| l.split(' ').next() == Some(d)));
    let Some(target) = target else {
        eprintln!(
            "disks unlock: there's nothing to unlock{}",
            dev.map(|d| format!(" on {d}")).unwrap_or_default()
        );
        return 1;
    };
    let mut f = target.split(' ');
    let (d, label) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
    for attempt in 0..3 {
        let Some(mut secret) = ask(&format!(
            "Passphrase (or recovery key) for {label} on {d}: "
        )) else {
            return 1;
        };
        let mut t = format!("dev={d} {secret}");
        let r = run("unlock", d, "unlock", &t);
        wipe(&mut t);
        wipe(&mut secret);
        if r.is_ok() {
            println!("disks: {label} unlocked");
            return 0;
        }
        if attempt < 2 {
            eprintln!("disks: try again");
        }
    }
    1
}

fn lock(target: Option<&str>) -> i32 {
    if run(
        "lock",
        target.unwrap_or(""),
        "lock",
        target.unwrap_or("all"),
    )
    .is_err()
    {
        return 1;
    }
    // Every locked volume, the one just locked among them.
    for l in read(&ctl_path("lock"))
        .lines()
        .filter(|l| l.ends_with(" locked"))
    {
        println!("disks: {l}");
    }
    0
}

fn passphrase() -> i32 {
    let Some(mut old) = ask("Current passphrase (or recovery key): ") else {
        return 1;
    };
    let Some(mut new) = ask("New passphrase (8 characters or more): ") else {
        return 1;
    };
    let Some(mut again) = ask("New passphrase again: ") else {
        return 1;
    };
    if new != again {
        eprintln!("disks passphrase: the new passphrases differ");
        return 1;
    }
    let mut t = format!("{old}\n{new}");
    let r = run("passphrase", "", "passwd", &t);
    wipe(&mut t);
    wipe(&mut old);
    wipe(&mut new);
    wipe(&mut again);
    done(r, || {
        println!("disks: passphrase changed (the volume key was rewrapped)")
    })
}

// ---- send, receive --------------------------------------------------------------------------

/// Is the primary volume encrypted (its `info` says `encryption: on`)?
fn volume_encrypted() -> bool {
    read(&ctl_path("info"))
        .lines()
        .any(|l| l.starts_with("encryption:") && l.ends_with(" on"))
}

/// The kernel's report of the last send / receive.
fn report(op: &str) {
    let t = read(&ctl_path(op));
    if !t.trim_end().is_empty() {
        eprintln!("disks: {}", t.trim_end());
    }
}

/// A private temporary file for a stream: made here (O_EXCL, 0600) so nothing planted in
/// /tmp is followed and nobody else can read it; the kernel then writes into it.
fn private_tmp(what: &str) -> Result<(String, i32), String> {
    let path = format!("/tmp/.disks-{what}-{}", sys::getpid());
    sys::open_mode(
        &path,
        sys::O_WRONLY | sys::O_CREAT | sys::O_EXCL | sys::O_CLOEXEC,
        0o600,
    )
    .map(|fd| (path.clone(), fd))
    .map_err(|e| format!("can't make the temporary file {path}: {e}"))
}

/// Copy everything from `from` to `to` (fds).
fn pump(from: i32, to: i32) -> Result<u64, rt::Errno> {
    let mut buf = alloc::vec![0u8; 64 << 10];
    let mut n = 0u64;
    loop {
        let k = sys::read(from, &mut buf)?;
        if k == 0 {
            return Ok(n);
        }
        sys::write_all(to, &buf[..k])?;
        n += k as u64;
    }
}

/// `disks send SNAPSHOT [--from OLDER] [--output FILE] [--encrypt | --plaintext]
/// [--passphrase-file F]`: to FILE, or stdout.
fn send(p: &Parsed, snap: &str) -> i32 {
    let plaintext = p.has("plaintext");
    let pfile = p.value("passphrase-file");
    if plaintext && (p.has("encrypt") || pfile.is_some()) {
        eprintln!("disks send: --plaintext doesn't go with --encrypt or --passphrase-file");
        return 2;
    }
    let out = p.value("output");
    let tmp = if out.is_none() {
        match private_tmp("send") {
            Ok((path, fd)) => {
                let _ = sys::close(fd);
                Some(path)
            }
            Err(e) => {
                eprintln!("disks send: {e}");
                return 1;
            }
        }
    } else {
        None
    };
    let path = out.map(abs).or_else(|| tmp.clone()).unwrap_or_default();
    let mut req = match p.value("from") {
        Some(f) => format!("{snap} {f} {path}"),
        None => format!("{snap} {path}"),
    };
    // Encrypted when asked, or by default from an encrypted volume.
    let sealed = !plaintext && (p.has("encrypt") || pfile.is_some() || volume_encrypted());
    if plaintext {
        req.push_str(" plaintext");
    }
    let ok = if sealed {
        match stream_passphrase(pfile, true) {
            Some(mut pass) => {
                req.push('\n');
                req.push_str(&pass);
                let r = run("send", snap, "send", &req).is_ok();
                wipe(&mut req);
                wipe(&mut pass);
                r
            }
            None => false,
        }
    } else {
        run("send", snap, "send", &req).is_ok()
    };
    if !ok {
        if let Some(t) = &tmp {
            let _ = sys::unlink(t);
        }
        return 1;
    }
    report("send");
    if let Some(t) = &tmp {
        let r = sys::open(t, sys::O_RDONLY).and_then(|fd| {
            let r = pump(fd, 1);
            let _ = sys::close(fd);
            r
        });
        let _ = sys::unlink(t);
        if let Err(e) = r {
            eprintln!("disks send: couldn't write the stream out: {e}");
            return 1;
        }
    }
    0
}

/// Does the stream file start with the encrypted-stream envelope?
fn stream_sealed(path: &str) -> bool {
    let Ok(fd) = sys::open(path, sys::O_RDONLY) else {
        return false;
    };
    let mut m = [0u8; 8];
    let n = sys::read(fd, &mut m).unwrap_or(0);
    let _ = sys::close(fd);
    n == 8 && &m == b"MILKSTRX"
}

/// `disks receive [FILE] [--to PATH] [--force] [--passphrase-file F]`: recreate the snapshot
/// in a stream (FILE or stdin) on the milkfs volume PATH is on (default /Volumes/Milk);
/// --force rolls the target back to the stream's base first.
fn receive(p: &Parsed, file: Option<&str>) -> i32 {
    let pfile = p.value("passphrase-file");
    let mut tmp = None;
    let path = match file {
        Some(f) => abs(f),
        None => {
            // A stream on stdin: the kernel reads it from a file.
            let (path, fd) = match private_tmp("receive") {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("disks receive: {e}");
                    return 1;
                }
            };
            let r = pump(0, fd);
            let _ = sys::close(fd);
            tmp = Some(path.clone());
            if let Err(e) = r {
                eprintln!("disks receive: couldn't read the stream from stdin: {e}");
                let _ = sys::unlink(&path);
                return 1;
            }
            path
        }
    };
    let mut req = path.clone();
    if let Some(t) = p.value("to") {
        req = format!("{req} {}", abs(t));
    }
    if p.has("force") {
        req.push_str(" force");
    }
    let r = if !stream_sealed(&path) {
        run("receive", "", "receive", &req).is_ok()
    } else if file.is_none() && pfile.is_none() {
        eprintln!(
            "disks receive: the stream on stdin is encrypted: give its passphrase with --passphrase-file FILE"
        );
        false
    } else {
        let mut ok = false;
        for attempt in 0..if pfile.is_some() { 1 } else { 3 } {
            let Some(mut pass) = stream_passphrase(pfile, false) else {
                break;
            };
            let mut t = format!("{req}\n{pass}");
            let r = write_ctl("receive", &t);
            wipe(&mut t);
            wipe(&mut pass);
            match r {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) if e.0 == 13 && attempt < 2 && pfile.is_none() => {
                    eprintln!("disks receive: {}", why("receive", e));
                    eprintln!("disks: try again")
                }
                Err(e) => {
                    eprintln!("disks receive: {}", why("receive", e));
                    break;
                }
            }
        }
        ok
    };
    if let Some(t) = &tmp {
        let _ = sys::unlink(t);
    }
    if r {
        report("receive");
        0
    } else {
        1
    }
}

// ---- dedupe ---------------------------------------------------------------------------------

/// `disks dedupe [--dry-run] PATH`: identical files under PATH (on milkfs) share their data
/// blocks (copy-on-write), so the copies take no space until one is changed. Files are
/// grouped by size and content hash (milkfs's block checksums, no data read), compared byte
/// by byte, then all but the first of a group get its blocks. Times and owners stay.
fn dedupe(dir: &str, dry: bool) -> i32 {
    let path = abs(dir);
    let req = if dry {
        format!("{path} dry")
    } else {
        path.clone()
    };
    if let Err(e) = write_ctl("dedupe", &req) {
        let why = match e.0 {
            95 => format!("{dir} isn't on a milkfs volume"),
            _ => filekit::words::say(dir, e.0),
        };
        eprintln!("disks dedupe: {why}");
        return 1;
    }
    show("dedupe", "")
}
