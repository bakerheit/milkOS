//! milk: the milkOS package manager. Packages are .milkpkg files (ustar + manifest, see
//! scripts/mkpkg.py and docs/packages.md) from repositories listed in /etc/milk/repos.
//!
//!   milk update               fetch every repository's index.txt
//!   milk search [WORD]        packages whose name or description contains WORD
//!   milk info NAME            index entry, and the installed manifest if installed
//!   milk install NAME         download, verify (sha256 of the file, then of the payload),
//!                             unpack into /Applications/NAME/, link /bin/NAME
//!   milk remove NAME          delete /Applications/NAME and its /bin link
//!   milk list                 installed packages
//!   milk try NAME             install into a milkfs clone of a fresh snapshot only; the
//!                             live system is untouched (run it from the clone's path)
//!   milk keep NAME            promote that clone: the install becomes real
//!   milk discard NAME         throw the clone away
//!   milk relink [-q]          recreate the /bin links (/bin is in RAM; init runs this)
//!
//! Every install/remove/try/keep/discard is an audit record (`user.pkg.*`, written through
//! /dev/audit, which the kernel turns into records; the snapshot/clone/promote steps are
//! recorded by the kernel itself as milkfs.*). SHA-256: rt::sha256, the same code `audit`
//! verifies the log with.

#![no_std]
#![no_main]

extern crate alloc;

mod http;
mod tar;

use alloc::string::String;
use alloc::vec::Vec;
use rt::sha256::{Sha256, hex, sha256};
use rt::{eprintln, println, sys};

rt::main!(main);

const REPOS: &str = "/etc/milk/repos";
const APPS: &str = "/Applications";
const CACHE_DIR: &str = "/Applications/.milk";
const CACHE: &str = "/Applications/.milk/index.txt";
const MILKFS: &str = "/sys/fs/milkfs";

type Res<T> = Result<T, String>;

// ---- small file helpers ---------------------------------------------------------------

fn write_file(path: &str, data: &[u8], mode: u32) -> Res<()> {
    let fd = sys::open_mode(
        path,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        mode,
    )
    .map_err(|e| alloc::format!("{path}: {e}"))?;
    let r = sys::write_all(fd, data).map_err(|e| alloc::format!("{path}: {e}"));
    let _ = sys::close(fd);
    r
}

fn mkdir_all(path: &str) -> Res<()> {
    let mut cur = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        cur.push('/');
        cur.push_str(part);
        if sys::stat_path(&cur, true).is_err() {
            sys::mkdir(&cur, 0o755).map_err(|e| alloc::format!("mkdir {cur}: {e}"))?;
        }
    }
    Ok(())
}

fn exists(path: &str) -> bool {
    sys::stat_path(path, false).is_ok()
}

fn rm_rf(path: &str) -> Res<()> {
    let st = match sys::stat_path(path, false) {
        Ok(st) => st,
        Err(_) => return Ok(()),
    };
    if st.is_dir() {
        for (name, _) in sys::read_dir(path).map_err(|e| alloc::format!("{path}: {e}"))? {
            if name != "." && name != ".." {
                rm_rf(&alloc::format!("{path}/{name}"))?;
            }
        }
        sys::rmdir(path).map_err(|e| alloc::format!("rmdir {path}: {e}"))
    } else {
        sys::unlink(path).map_err(|e| alloc::format!("rm {path}: {e}"))
    }
}

fn text(path: &str) -> Option<String> {
    sys::read_file(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Write a control file under /sys/fs/milkfs.
fn milkfs(ctl: &str, arg: &str) -> Res<()> {
    let p = alloc::format!("{MILKFS}/{ctl}");
    let fd =
        sys::open(&p, sys::O_WRONLY | sys::O_CLOEXEC).map_err(|e| alloc::format!("{p}: {e}"))?;
    let r = sys::write_all(fd, arg.as_bytes()).map_err(|e| alloc::format!("{p} <- {arg:?}: {e}"));
    let _ = sys::close(fd);
    r
}

/// An audit record through /dev/audit (the kernel adds pid, exe, time, the hash chain).
fn audit(event: &str, detail: &str) {
    if let Ok(fd) = sys::open("/dev/audit", sys::O_WRONLY | sys::O_CLOEXEC) {
        let _ = sys::write_all(fd, alloc::format!("{event} {detail}\n").as_bytes());
        let _ = sys::close(fd);
    }
}

// ---- index ------------------------------------------------------------------------------

struct Pkg {
    repo: String,
    name: String,
    version: String,
    sha256: String,
    size: usize,
    file: String,
    desc: String,
}

fn repos() -> Vec<String> {
    text(REPOS)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// The cached index: `<repo> <name> <version> <sha256> <size> <file> <description...>`.
fn index() -> Vec<Pkg> {
    text(CACHE)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut w = l.splitn(7, ' ');
            Some(Pkg {
                repo: String::from(w.next()?),
                name: String::from(w.next()?),
                version: String::from(w.next()?),
                sha256: String::from(w.next()?),
                size: w.next()?.parse().ok()?,
                file: String::from(w.next()?),
                desc: String::from(w.next().unwrap_or("")),
            })
        })
        .collect()
}

fn find(name: &str) -> Res<Pkg> {
    let idx = index();
    if idx.is_empty() {
        return Err(String::from("no package index; run `milk update` first"));
    }
    idx.into_iter()
        .find(|p| p.name == name)
        .ok_or_else(|| alloc::format!("{name}: no such package (milk search lists them)"))
}

fn cmd_update() -> Res<()> {
    let list = repos();
    if list.is_empty() {
        return Err(alloc::format!("no repositories in {REPOS}"));
    }
    let mut out = String::new();
    let mut total = 0;
    for r in &list {
        let url = http::parse_url(r)?;
        let body = http::get(&url, "index.txt").map_err(|e| alloc::format!("{r}: {e}"))?;
        let t = String::from_utf8_lossy(&body);
        let mut n = 0;
        for l in t
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            if l.split(' ').count() < 5 {
                continue;
            }
            out.push_str(&alloc::format!("{r} {l}\n"));
            n += 1;
        }
        println!("milk: {r}: {n} packages");
        total += n;
    }
    mkdir_all(CACHE_DIR)?;
    write_file(CACHE, out.as_bytes(), 0o644)?;
    println!("milk: index updated, {total} packages");
    Ok(())
}

fn installed(root: &str) -> Vec<(String, Manifest)> {
    let mut v: Vec<(String, Manifest)> = sys::read_dir(root)
        .unwrap_or_default()
        .into_iter()
        .filter(|(n, _)| !n.starts_with('.'))
        .filter_map(|(n, _)| {
            let m = Manifest::parse(&text(&alloc::format!("{root}/{n}/manifest"))?)?;
            Some((n, m))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

// ---- packages -----------------------------------------------------------------------------

struct Manifest {
    name: String,
    version: String,
    description: String,
    exec: Option<String>,
    dock: Option<String>,
    files: Vec<String>,
    payload_sha256: String,
}

impl Manifest {
    fn parse(t: &str) -> Option<Manifest> {
        let get = |k: &str| {
            t.lines()
                .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(' ')))
                .map(String::from)
        };
        if get("format").as_deref() != Some("1") {
            return None;
        }
        Some(Manifest {
            name: get("name")?,
            version: get("version")?,
            description: get("description").unwrap_or_default(),
            exec: get("exec"),
            dock: get("dock"),
            files: get("files")
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect(),
            payload_sha256: get("payload-sha256")?,
        })
    }
}

/// Download `p`, check both hashes, unpack into `<apps_root>/<name>/`. Returns the
/// manifest.
fn fetch_and_unpack(p: &Pkg, apps_root: &str) -> Res<Manifest> {
    let url = http::parse_url(&p.repo)?;
    println!(
        "milk: downloading {} {} ({} bytes) from {}",
        p.name, p.version, p.size, p.repo
    );
    let data = http::get(&url, &p.file)?;
    let got = hex(&sha256(&data));
    if got != p.sha256 {
        return Err(alloc::format!(
            "{}: sha256 mismatch: index says {}, file is {} (not installed)",
            p.file,
            &p.sha256[..16.min(p.sha256.len())],
            &got[..16]
        ));
    }
    println!("milk: sha256 {}... ok", &got[..16]);
    let ents = tar::entries(&data)?;
    let (first, rest) = ents.split_first().ok_or("empty package")?;
    if first.path != "manifest" {
        return Err(String::from("package does not start with a manifest"));
    }
    let man_text = String::from_utf8_lossy(first.data).into_owned();
    let man = Manifest::parse(&man_text).ok_or("bad manifest")?;
    if man.name != p.name {
        return Err(alloc::format!(
            "package says it is {:?}, not {:?}",
            man.name,
            p.name
        ));
    }
    // Payload hash over the files in manifest order.
    let mut h = Sha256::new();
    for f in &man.files {
        let e = rest
            .iter()
            .find(|e| &e.path == f)
            .ok_or_else(|| alloc::format!("payload file {f} missing"))?;
        h.update(f.as_bytes());
        h.update(&[0]);
        h.update(&(e.data.len() as u64).to_le_bytes());
        h.update(e.data);
    }
    let ph = hex(&h.finish());
    if ph != man.payload_sha256 {
        return Err(String::from(
            "payload sha256 does not match the manifest (not installed)",
        ));
    }
    println!(
        "milk: payload sha256 {}... ok ({} files)",
        &ph[..16],
        man.files.len()
    );
    // Unpack next to the target, then swap it in.
    let dest = alloc::format!("{apps_root}/{}", man.name);
    let tmp = alloc::format!("{apps_root}/.{}.new", man.name);
    rm_rf(&tmp)?;
    for e in rest {
        let path = alloc::format!("{tmp}/{}", e.path);
        if let Some((dir, _)) = path.rsplit_once('/') {
            mkdir_all(dir)?;
        }
        write_file(
            &path,
            e.data,
            if e.mode & 0o111 != 0 { 0o755 } else { 0o644 },
        )?;
    }
    write_file(
        &alloc::format!("{tmp}/manifest"),
        man_text.as_bytes(),
        0o644,
    )?;
    write_file(
        &alloc::format!("{tmp}/.origin"),
        alloc::format!("repo {}\nfile {}\nsha256 {}\n", p.repo, p.file, got).as_bytes(),
        0o644,
    )?;
    rm_rf(&dest)?;
    sys::rename(&tmp, &dest).map_err(|e| alloc::format!("rename {tmp} -> {dest}: {e}"))?;
    Ok(man)
}

/// /bin/<basename(exec)> -> /Applications/<name>/<exec>, unless /bin has something else
/// by that name.
fn link(man: &Manifest, quiet: bool) -> Res<()> {
    let Some(exec) = &man.exec else { return Ok(()) };
    let base = exec.rsplit('/').next().unwrap_or(exec);
    let target = alloc::format!("{APPS}/{}/{exec}", man.name);
    let bin = alloc::format!("/bin/{base}");
    match sys::readlink(&bin) {
        Ok(t) if t == target => return Ok(()),
        Ok(t) if t.starts_with("/Applications/") => {
            let _ = sys::unlink(&bin);
        }
        _ if exists(&bin) => {
            return Err(alloc::format!(
                "{bin} exists and is not a package link; not touching it"
            ));
        }
        _ => {}
    }
    sys::symlink(&target, &bin).map_err(|e| alloc::format!("ln -s {target} {bin}: {e}"))?;
    if !quiet {
        println!("milk: {bin} -> {target}");
    }
    Ok(())
}

fn unlink_bin(man: &Manifest) {
    if let Some(exec) = &man.exec {
        let base = exec.rsplit('/').next().unwrap_or(exec);
        let bin = alloc::format!("/bin/{base}");
        if sys::readlink(&bin).is_ok_and(|t| t.starts_with(&alloc::format!("{APPS}/{}/", man.name)))
        {
            let _ = sys::unlink(&bin);
        }
    }
}

fn cmd_install(name: &str) -> Res<()> {
    let p = find(name)?;
    let man = fetch_and_unpack(&p, APPS)?;
    link(&man, false)?;
    audit(
        "pkg.install",
        &alloc::format!(
            "name={} version={} sha256={} from={}{}",
            man.name,
            man.version,
            p.sha256,
            p.repo,
            p.file
        ),
    );
    println!(
        "milk: installed {} {} in {APPS}/{}",
        man.name, man.version, man.name
    );
    Ok(())
}

fn cmd_remove(name: &str) -> Res<()> {
    let dir = alloc::format!("{APPS}/{name}");
    let man = text(&alloc::format!("{dir}/manifest"))
        .and_then(|t| Manifest::parse(&t))
        .ok_or_else(|| alloc::format!("{name} is not installed"))?;
    unlink_bin(&man);
    rm_rf(&dir)?;
    audit(
        "pkg.remove",
        &alloc::format!("name={} version={}", man.name, man.version),
    );
    println!("milk: removed {} {}", man.name, man.version);
    Ok(())
}

fn cmd_list() {
    let v = installed(APPS);
    if v.is_empty() {
        println!("milk: no packages installed");
    }
    for (_, m) in v {
        println!("{:<10} {:<8} {}", m.name, m.version, m.description);
    }
}

fn cmd_search(word: Option<&str>) -> Res<()> {
    let idx = index();
    if idx.is_empty() {
        return Err(String::from("no package index; run `milk update` first"));
    }
    let inst = installed(APPS);
    for p in idx {
        if let Some(w) = word
            && !p.name.contains(w)
            && !p.desc.to_lowercase().contains(&w.to_lowercase())
        {
            continue;
        }
        let mark = if inst.iter().any(|(n, _)| *n == p.name) {
            "[installed]"
        } else {
            ""
        };
        println!("{:<10} {:<8} {} {mark}", p.name, p.version, p.desc);
    }
    Ok(())
}

fn cmd_info(name: &str) -> Res<()> {
    let p = find(name);
    if let Ok(p) = &p {
        println!("name:        {}", p.name);
        println!("version:     {}", p.version);
        println!("description: {}", p.desc);
        println!("size:        {} bytes", p.size);
        println!("sha256:      {}", p.sha256);
        println!("from:        {}{}", p.repo, p.file);
    }
    match text(&alloc::format!("{APPS}/{name}/manifest")).and_then(|t| Manifest::parse(&t)) {
        Some(m) => {
            println!("installed:   {} in {APPS}/{name}", m.version);
            if let Some(e) = &m.exec {
                println!("runs as:     /bin/{}", e.rsplit('/').next().unwrap_or(e));
            }
            if let Some(d) = &m.dock {
                println!("dock:        {d}");
            }
            println!("files:       {}", m.files.join(" "));
        }
        None => {
            p?;
            println!("installed:   no");
        }
    }
    Ok(())
}

// ---- try before install (milkfs clones) ---------------------------------------------------

fn clone_name(name: &str) -> String {
    alloc::format!("try-{name}")
}
fn snap_name(name: &str) -> String {
    alloc::format!("milk-try-{name}")
}

/// Mount path of clone `c` (from /sys/fs/milkfs/clones: `<name> of=<snap> at=<path>`).
fn clone_path(c: &str) -> Option<String> {
    text(&alloc::format!("{MILKFS}/clones"))?
        .lines()
        .find(|l| l.split(' ').next() == Some(c))
        .and_then(|l| {
            l.split(' ')
                .find_map(|w| w.strip_prefix("at="))
                .map(String::from)
        })
        .filter(|p| p != "-")
}

fn cmd_try(name: &str) -> Res<()> {
    let p = find(name)?;
    let (c, snap) = (clone_name(name), snap_name(name));
    if clone_path(&c).is_some() {
        return Err(alloc::format!(
            "already trying {name}: `milk keep {name}` or `milk discard {name}`"
        ));
    }
    // A fresh snapshot of the live system to branch from.
    if text(&alloc::format!("{MILKFS}/snapshots"))
        .unwrap_or_default()
        .lines()
        .any(|l| l.split(' ').next() == Some(snap.as_str()))
    {
        let _ = milkfs("delete", &snap);
    }
    milkfs("snapshot", &snap)?;
    milkfs("clone", &alloc::format!("{snap} {c}"))?;
    let root = clone_path(&c).ok_or("clone did not mount")?;
    let apps = alloc::format!("{root}{APPS}");
    mkdir_all(&apps)?;
    let man = match fetch_and_unpack(&p, &apps) {
        Ok(m) => m,
        Err(e) => {
            let _ = milkfs("discard", &c);
            let _ = milkfs("delete", &snap);
            return Err(e);
        }
    };
    audit(
        "pkg.try",
        &alloc::format!(
            "name={} version={} sha256={} clone={c} at={root} snapshot={snap}",
            man.name,
            man.version,
            p.sha256
        ),
    );
    println!(
        "milk: {} {} is installed in clone {c} only; the live system is untouched.",
        man.name, man.version
    );
    if let Some(e) = &man.exec {
        println!("milk: try it:   {apps}/{}/{e}", man.name);
    }
    println!("milk: then:     milk keep {name}  (make it real)  or  milk discard {name}");
    Ok(())
}

fn cmd_keep(name: &str) -> Res<()> {
    let c = clone_name(name);
    clone_path(&c).ok_or_else(|| alloc::format!("not trying {name} (no clone {c})"))?;
    milkfs("promote", &c)?;
    // The origin snapshot is not needed any more (before-<clone> holds the undo).
    let _ = milkfs("delete", &snap_name(name));
    // The promoted clone is the live volume now; /Applications shows it.
    let man = text(&alloc::format!("{APPS}/{name}/manifest"))
        .and_then(|t| Manifest::parse(&t))
        .ok_or_else(|| alloc::format!("{name} not found in {APPS} after promoting {c}"))?;
    link(&man, false)?;
    audit(
        "pkg.keep",
        &alloc::format!(
            "name={} version={} clone={c} undo=rollback:before-{c}",
            man.name,
            man.version
        ),
    );
    println!(
        "milk: kept {} {}: clone {c} is now the live system (undo: echo before-{c} > {MILKFS}/rollback)",
        man.name, man.version
    );
    Ok(())
}

fn cmd_discard(name: &str) -> Res<()> {
    let c = clone_name(name);
    clone_path(&c).ok_or_else(|| alloc::format!("not trying {name} (no clone {c})"))?;
    milkfs("discard", &c)?;
    let _ = milkfs("delete", &snap_name(name));
    audit("pkg.discard", &alloc::format!("name={name} clone={c}"));
    println!("milk: discarded clone {c}; the live system never saw {name}");
    Ok(())
}

fn cmd_relink(quiet: bool) {
    for (_, m) in installed(APPS) {
        if let Err(e) = link(&m, quiet) {
            eprintln!("milk: {e}");
        }
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage: milk update | search [WORD] | info NAME | install NAME | remove NAME | list\n       milk try NAME | keep NAME | discard NAME | relink [-q]"
    );
    2
}

fn main(args: &rt::Args) -> i32 {
    let a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    let r = match a.as_slice() {
        ["update"] => cmd_update(),
        ["search"] => cmd_search(None),
        ["search", w] => cmd_search(Some(w)),
        ["info", n] => cmd_info(n),
        ["install", n] => cmd_install(n),
        ["remove", n] => cmd_remove(n),
        ["list"] => {
            cmd_list();
            Ok(())
        }
        ["try", n] => cmd_try(n),
        ["keep", n] => cmd_keep(n),
        ["discard", n] => cmd_discard(n),
        ["relink"] => {
            cmd_relink(false);
            Ok(())
        }
        ["relink", "-q"] => {
            cmd_relink(true);
            Ok(())
        }
        _ => return usage(),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("milk: {e}");
            1
        }
    }
}
