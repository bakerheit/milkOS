//! milk: the milkOS package manager. Packages are .milkpkg files (ustar + manifest, see
//! scripts/mkpkg.py and docs/packages.md) from repositories listed in /etc/milk/repos.
//!
//!   milk update               fetch every repository's index.txt
//!   milk search [WORD]        packages whose name or description contains WORD
//!   milk info NAME            index entry, and the installed manifest if installed
//!   milk install NAME[=VER]   download, verify (signature, sha256 of the file, then of the
//!                             payload), dependencies first; unpack into /Applications/NAME/,
//!                             link /bin/NAME. The newest version unless =VER.
//!   milk remove [--force] NAME  delete /Applications/NAME and its /bin link (refused while
//!                             another installed package depends on it, unless --force)
//!   milk autoremove           remove packages installed only as dependencies that nothing
//!                             needs any more
//!   milk upgrade [NAME] [-y]  newer versions into one clone (try-upgrade) as a transaction:
//!                             a failure discards it; then `milk keep upgrade` or
//!                             `milk discard upgrade` (-y: keep at once)
//!   milk list                 installed packages
//!   milk try NAME             install into a milkfs clone of a fresh snapshot only; the
//!                             live system is untouched (run it from the clone's path)
//!   milk keep NAME            promote that clone: the install becomes real
//!   milk discard NAME         throw the clone away
//!   milk relink [-q]          recreate the /bin links (/bin is in RAM; init runs this)
//!   milk keys                 trusted signing keys (/etc/milk/keys/*.pub)
//!   milk selftest             SHA-256, SHA-512, Ed25519 (RFC 8032) test vectors
//!
//! Signatures: index.txt and every package must carry a valid Ed25519 signature
//! (`<file>.sig`) by a key in /etc/milk/keys, or milk refuses them (and writes a
//! `user.pkg.verify_fail` audit record); `--insecure` accepts them anyway, still audited.
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
mod version;

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
const KEYS: &str = "/etc/milk/keys";

/// --insecure: accept unsigned / badly signed indexes and packages (loudly, and audited).
static INSECURE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn insecure() -> bool {
    INSECURE.load(core::sync::atomic::Ordering::Relaxed)
}

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

// ---- signatures (Ed25519, rt::ed25519) ------------------------------------------------------

fn key_name_ok(n: &str) -> bool {
    (1..=32).contains(&n.len())
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// The trusted public key `/etc/milk/keys/<name>.pub` (`ed25519 <hex> <name>`).
fn trusted_key(name: &str) -> Option<[u8; 32]> {
    if !key_name_ok(name) {
        return None;
    }
    let t = text(&alloc::format!("{KEYS}/{name}.pub"))?;
    t.lines().find_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        (w.len() >= 2 && w[0] == "ed25519")
            .then(|| rt::ed25519::unhex::<32>(w[1]))
            .flatten()
    })
}

/// Check `sig` (the `.sig` file: `ed25519 <key> <hex>`) over `data`. Ok(key name); on any
/// failure an audit record, and Err unless --insecure.
fn check_sig(what: &str, data: &[u8], sig: Result<Vec<u8>, String>, repo: &str) -> Res<String> {
    let verdict = (|| -> Result<String, String> {
        let sig = sig.map_err(|e| alloc::format!("unsigned (no .sig: {e})"))?;
        let t = String::from_utf8_lossy(&sig).into_owned();
        let w: Vec<&str> = t.split_whitespace().collect();
        let [alg, key, hexsig] = w[..] else {
            return Err(String::from("malformed .sig"));
        };
        if alg != "ed25519" {
            return Err(alloc::format!("unknown signature algorithm {alg:?}"));
        }
        let pk = trusted_key(key)
            .ok_or_else(|| alloc::format!("key {key:?} is not trusted (not in {KEYS})"))?;
        let s = rt::ed25519::unhex::<64>(hexsig).ok_or("malformed signature")?;
        if rt::ed25519::verify(&pk, data, &s) {
            Ok(String::from(key))
        } else {
            Err(alloc::format!("BAD signature (key {key})"))
        }
    })();
    match verdict {
        Ok(k) => {
            println!("milk: {what}: signature ok (key {k})");
            Ok(k)
        }
        Err(why) => {
            audit(
                "pkg.verify_fail",
                &alloc::format!(
                    "what={what} repo={repo} reason={why} insecure={}",
                    insecure()
                ),
            );
            if insecure() {
                eprintln!("milk: WARNING: {what}: {why}; continuing because of --insecure");
                Ok(String::from("INSECURE"))
            } else {
                Err(alloc::format!(
                    "{what}: {why}; refused (milk --insecure ... to override)"
                ))
            }
        }
    }
}

fn cmd_keys() {
    let mut any = false;
    for (n, _) in sys::read_dir(KEYS).unwrap_or_default() {
        if let Some(name) = n.strip_suffix(".pub")
            && let Some(k) = trusted_key(name)
        {
            println!("{name:<16} ed25519 {}", hex_bytes(&k));
            any = true;
        }
    }
    if !any {
        println!("milk: no trusted keys in {KEYS}: every package will be refused");
    }
}

fn hex_bytes(b: &[u8]) -> String {
    let mut s = String::new();
    for x in b {
        s.push_str(&alloc::format!("{x:02x}"));
    }
    s
}

fn cmd_selftest() -> Res<()> {
    let (a, an) = rt::sha256::selftest();
    let (b, bn) = rt::sha512::selftest();
    let (c, cn) = rt::ed25519::selftest();
    let (d, dn) = version::selftest();
    println!("milk: version compare {d}/{dn} checks");
    println!(
        "milk: sha256 {a}/{an} FIPS 180-4, sha512 {b}/{bn} FIPS 180-4, ed25519 {c}/{cn} RFC 8032 checks"
    );
    if a == an && b == bn && c == cn && d == dn {
        println!("milk: selftest ok");
        Ok(())
    } else {
        Err(String::from("selftest FAILED"))
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

/// The newest version of `name` in the index, or exactly `name=VER`, or the newest that
/// satisfies `min`.
fn find_in(idx: &[Pkg], spec: &str, min: Option<&str>) -> Res<Pkg> {
    if idx.is_empty() {
        return Err(String::from("no package index; run `milk update` first"));
    }
    let (name, exact) = spec
        .split_once('=')
        .map_or((spec, None), |(n, v)| (n, Some(v)));
    idx.iter()
        .filter(|p| p.name == name)
        .filter(|p| exact.is_none_or(|v| p.version == v))
        .filter(|p| min.is_none_or(|m| version::cmp(&p.version, m) != core::cmp::Ordering::Less))
        .max_by(|a, b| version::cmp(&a.version, &b.version))
        .map(|p| Pkg {
            repo: p.repo.clone(),
            name: p.name.clone(),
            version: p.version.clone(),
            sha256: p.sha256.clone(),
            size: p.size,
            file: p.file.clone(),
            desc: p.desc.clone(),
        })
        .ok_or_else(|| match (exact, min) {
            (Some(v), _) => alloc::format!("{name} {v}: no such version in the index"),
            (None, Some(m)) => alloc::format!("{name}: nothing >= {m} in the index"),
            _ => alloc::format!("{name}: no such package (milk search lists them)"),
        })
}

fn find(spec: &str) -> Res<Pkg> {
    find_in(&index(), spec, None)
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
        check_sig(
            &alloc::format!("{r}index.txt"),
            &body,
            http::get(&url, "index.txt.sig"),
            r,
        )?;
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
    depends: Vec<version::Dep>,
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
            depends: get("depends")
                .unwrap_or_default()
                .split_whitespace()
                .map(version::Dep::parse)
                .collect(),
        })
    }
}

/// A package downloaded and verified (signature, index sha256, payload sha256).
struct Fetched {
    data: Vec<u8>,
    man: Manifest,
    man_text: String,
    sha: String,
}

/// Download `p` and check it: signature, then the index's sha256, then the manifest's
/// payload sha256. Nothing is written.
fn fetch_verified(p: &Pkg) -> Res<Fetched> {
    let url = http::parse_url(&p.repo)?;
    println!(
        "milk: downloading {} {} ({} bytes) from {}",
        p.name, p.version, p.size, p.repo
    );
    let data = http::get(&url, &p.file)?;
    // Signature first (it covers the bytes), then the index's sha256.
    check_sig(
        &p.file,
        &data,
        http::get(&url, &alloc::format!("{}.sig", p.file)),
        &p.repo,
    )?;
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
    let (man_text, man) = {
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
        (man_text, man)
    };
    Ok(Fetched {
        data,
        man,
        man_text,
        sha: got,
    })
}

/// Unpack a verified package into `<apps_root>/<name>/` (next to it first, then swapped in).
/// `auto`: installed only as a dependency (`milk autoremove` may take it away).
fn unpack(p: &Pkg, f: &Fetched, apps_root: &str, auto: bool) -> Res<()> {
    let ents = tar::entries(&f.data)?;
    let dest = alloc::format!("{apps_root}/{}", f.man.name);
    let tmp = alloc::format!("{apps_root}/.{}.new", f.man.name);
    rm_rf(&tmp)?;
    for e in &ents[1..] {
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
    mkdir_all(&tmp)?;
    write_file(
        &alloc::format!("{tmp}/manifest"),
        f.man_text.as_bytes(),
        0o644,
    )?;
    write_file(
        &alloc::format!("{tmp}/.origin"),
        alloc::format!("repo {}\nfile {}\nsha256 {}\n", p.repo, p.file, f.sha).as_bytes(),
        0o644,
    )?;
    if auto {
        write_file(
            &alloc::format!("{tmp}/.auto"),
            b"installed as a dependency\n",
            0o644,
        )?;
    }
    rm_rf(&dest)?;
    sys::rename(&tmp, &dest).map_err(|e| alloc::format!("rename {tmp} -> {dest}: {e}"))?;
    Ok(())
}

fn installed_version(root: &str, name: &str) -> Option<String> {
    text(&alloc::format!("{root}/{name}/manifest"))
        .and_then(|t| Manifest::parse(&t))
        .map(|m| m.version)
}

fn is_auto(root: &str, name: &str) -> bool {
    exists(&alloc::format!("{root}/{name}/.auto"))
}

/// What one install transaction did: (name, version, sha256, repo+file, auto).
type Done = Vec<(String, String, String, String, bool)>;

/// Install `spec` (`name`, `name=VER`) into `root` (an /Applications directory), its
/// dependencies first (depth first, so the order is always dependencies before
/// dependents; cycles are refused). `min`: the version a dependent needs. `auto`: as a
/// dependency. `upgrade`: replace an installed older version. Returns what was installed.
fn install_tx(
    root: &str,
    spec: &str,
    min: Option<&str>,
    auto: bool,
    upgrade: bool,
    visiting: &mut Vec<String>,
    done: &mut Done,
) -> Res<()> {
    let idx = index();
    let p = find_in(&idx, spec, min)?;
    if visiting.contains(&p.name) {
        return Err(alloc::format!(
            "dependency cycle: {} -> {}",
            visiting.join(" -> "),
            p.name
        ));
    }
    if done.iter().any(|d| d.0 == p.name) {
        return Ok(());
    }
    if let Some(have) = installed_version(root, &p.name) {
        let newer = version::cmp(&p.version, &have) == core::cmp::Ordering::Greater;
        let exact = spec.contains('=');
        let enough = min.is_none_or(|m| version::cmp(&have, m) != core::cmp::Ordering::Less);
        if (have == p.version || (!upgrade && !exact && enough) || (!newer && !exact)) && enough {
            if !auto && is_auto(root, &p.name) {
                let _ = sys::unlink(&alloc::format!("{root}/{}/.auto", p.name));
                println!(
                    "milk: {} {} is installed (now marked as wanted)",
                    p.name, have
                );
            } else if !auto {
                println!("milk: {} {} is already installed", p.name, have);
            }
            return Ok(());
        }
    }
    let f = fetch_verified(&p)?;
    visiting.push(p.name.clone());
    for d in &f.man.depends {
        let ok = installed_version(root, &d.name).is_some_and(|v| d.ok(&v))
            && !done.iter().any(|x| x.0 == d.name);
        if !ok {
            println!(
                "milk: {} needs {}{}",
                p.name,
                d.name,
                d.min
                    .as_deref()
                    .map(|m| alloc::format!(" >= {m}"))
                    .unwrap_or_default()
            );
            install_tx(
                root,
                &d.name,
                d.min.as_deref(),
                true,
                upgrade,
                visiting,
                done,
            )?;
        }
    }
    visiting.pop();
    // Keep "wanted" packages wanted across upgrades.
    let keep_auto = auto && (installed_version(root, &p.name).is_none() || is_auto(root, &p.name));
    unpack(&p, &f, root, keep_auto)?;
    done.push((
        p.name.clone(),
        p.version.clone(),
        p.sha256.clone(),
        alloc::format!("{}{}", p.repo, p.file),
        keep_auto,
    ));
    Ok(())
}

/// Installed packages (at `root`) that depend on `name`.
fn dependents(root: &str, name: &str) -> Vec<String> {
    installed(root)
        .into_iter()
        .filter(|(n, m)| n != name && m.depends.iter().any(|d| d.name == name))
        .map(|(n, _)| n)
        .collect()
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

fn cmd_install(spec: &str) -> Res<()> {
    let mut done = Vec::new();
    install_tx(APPS, spec, None, false, false, &mut Vec::new(), &mut done)?;
    for (name, ver, sha, from, auto) in &done {
        if let Some(m) =
            text(&alloc::format!("{APPS}/{name}/manifest")).and_then(|t| Manifest::parse(&t))
        {
            link(&m, false)?;
        }
        audit(
            "pkg.install",
            &alloc::format!("name={name} version={ver} sha256={sha} from={from} auto={auto}"),
        );
        println!(
            "milk: installed {name} {ver} in {APPS}/{name}{}",
            if *auto { " (as a dependency)" } else { "" }
        );
    }
    Ok(())
}

fn remove_one(name: &str, why: &str) -> Res<()> {
    let dir = alloc::format!("{APPS}/{name}");
    let man = text(&alloc::format!("{dir}/manifest"))
        .and_then(|t| Manifest::parse(&t))
        .ok_or_else(|| alloc::format!("{name} is not installed"))?;
    unlink_bin(&man);
    rm_rf(&dir)?;
    audit(
        "pkg.remove",
        &alloc::format!("name={} version={}{why}", man.name, man.version),
    );
    println!("milk: removed {} {}", man.name, man.version);
    Ok(())
}

fn cmd_remove(name: &str, force: bool) -> Res<()> {
    let users = dependents(APPS, name);
    if !users.is_empty() && !force {
        return Err(alloc::format!(
            "{name} is needed by {} (remove that first, or milk remove --force {name})",
            users.join(", ")
        ));
    }
    remove_one(name, "")
}

/// Remove packages installed only as dependencies that nothing needs any more.
fn cmd_autoremove() -> Res<()> {
    let mut n = 0;
    loop {
        let victim = installed(APPS)
            .into_iter()
            .map(|(name, _)| name)
            .find(|name| is_auto(APPS, name) && dependents(APPS, name).is_empty());
        let Some(v) = victim else { break };
        remove_one(&v, " reason=autoremove")?;
        n += 1;
    }
    println!("milk: autoremove: {n} package(s) removed");
    Ok(())
}

fn cmd_list() {
    let v = installed(APPS);
    if v.is_empty() {
        println!("milk: no packages installed");
    }
    for (name, m) in v {
        let auto = if is_auto(APPS, &name) { " (auto)" } else { "" };
        println!("{:<10} {:<8} {}{auto}", m.name, m.version, m.description);
    }
}

fn cmd_search(word: Option<&str>) -> Res<()> {
    let idx = index();
    if idx.is_empty() {
        return Err(String::from("no package index; run `milk update` first"));
    }
    let mut names: Vec<&str> = idx.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let p = find_in(&idx, name, None)?;
        if let Some(w) = word
            && !p.name.contains(w)
            && !p.desc.to_lowercase().contains(&w.to_lowercase())
        {
            continue;
        }
        let mark = match installed_version(APPS, name) {
            Some(v) if v == p.version => String::from("[installed]"),
            Some(v) => alloc::format!("[installed {v}, upgrade available]"),
            None => String::new(),
        };
        println!("{:<10} {:<8} {} {mark}", p.name, p.version, p.desc);
    }
    Ok(())
}

fn cmd_info(name: &str) -> Res<()> {
    let p = find(name);
    if let Ok(p) = &p {
        let versions: Vec<String> = index()
            .into_iter()
            .filter(|x| x.name == p.name)
            .map(|x| x.version)
            .collect();
        println!("name:        {}", p.name);
        println!(
            "version:     {} (available: {})",
            p.version,
            versions.join(" ")
        );
        println!("description: {}", p.desc);
        println!("size:        {} bytes", p.size);
        println!("sha256:      {}", p.sha256);
        println!("from:        {}{}", p.repo, p.file);
    }
    match text(&alloc::format!("{APPS}/{name}/manifest")).and_then(|t| Manifest::parse(&t)) {
        Some(m) => {
            let auto = if is_auto(APPS, name) {
                " (as a dependency)"
            } else {
                ""
            };
            println!("installed:   {} in {APPS}/{name}{auto}", m.version);
            if let Some(e) = &m.exec {
                println!("runs as:     /bin/{}", e.rsplit('/').next().unwrap_or(e));
            }
            if let Some(d) = &m.dock {
                println!("dock:        {d}");
            }
            if !m.depends.is_empty() {
                let d: Vec<String> = m
                    .depends
                    .iter()
                    .map(|d| match &d.min {
                        Some(v) => alloc::format!("{}>={v}", d.name),
                        None => d.name.clone(),
                    })
                    .collect();
                println!("depends:     {}", d.join(" "));
            }
            let users = dependents(APPS, name);
            if !users.is_empty() {
                println!("needed by:   {}", users.join(" "));
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

/// Make a fresh snapshot + clone for `label`; returns the clone's /Applications path.
fn open_clone(label: &str) -> Res<(String, String)> {
    let (c, snap) = (clone_name(label), snap_name(label));
    if clone_path(&c).is_some() {
        return Err(alloc::format!(
            "already trying {label}: `milk keep {label}` or `milk discard {label}`"
        ));
    }
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
    Ok((root, apps))
}

fn close_clone(label: &str) {
    let _ = milkfs("discard", &clone_name(label));
    let _ = milkfs("delete", &snap_name(label));
}

fn cmd_try(name: &str) -> Res<()> {
    let p = find(name)?;
    let (c, snap) = (clone_name(name), snap_name(name));
    let (root, apps) = open_clone(name)?;
    let mut done = Vec::new();
    if let Err(e) = install_tx(&apps, name, None, false, true, &mut Vec::new(), &mut done) {
        close_clone(name);
        return Err(e);
    }
    let man = text(&alloc::format!("{apps}/{}/manifest", p.name))
        .and_then(|t| Manifest::parse(&t))
        .ok_or("installed package has no manifest")?;
    let others: Vec<String> = done
        .iter()
        .filter(|d| d.0 != p.name)
        .map(|d| alloc::format!("{} {}", d.0, d.1))
        .collect();
    audit(
        "pkg.try",
        &alloc::format!(
            "name={} version={} sha256={} clone={c} at={root} snapshot={snap}{}",
            man.name,
            man.version,
            p.sha256,
            if others.is_empty() {
                String::new()
            } else {
                alloc::format!(" with={}", others.join(","))
            }
        ),
    );
    println!(
        "milk: {} {} is installed in clone {c} only; the live system is untouched.",
        man.name, man.version
    );
    if !others.is_empty() {
        println!("milk: (with its dependencies: {})", others.join(", "));
    }
    if let Some(e) = &man.exec {
        println!("milk: try it:   {apps}/{}/{e}", man.name);
    }
    println!("milk: then:     milk keep {name}  (make it real)  or  milk discard {name}");
    Ok(())
}

/// `milk upgrade [name] [-y]`: newer versions (and new dependencies) go into one clone,
/// `try-upgrade`; any failure discards it, so the live system only ever sees a complete
/// upgrade. Without -y it stays there to try (`milk keep upgrade` / `milk discard
/// upgrade`); with -y it is kept at once.
fn cmd_upgrade(only: Option<&str>, yes: bool) -> Res<()> {
    let idx = index();
    let mut plan = Vec::new();
    for (name, m) in installed(APPS) {
        if only.is_some_and(|o| o != name) {
            continue;
        }
        if let Ok(p) = find_in(&idx, &name, None)
            && version::cmp(&p.version, &m.version) == core::cmp::Ordering::Greater
        {
            plan.push((name, m.version, p.version));
        }
    }
    if let Some(o) = only
        && installed_version(APPS, o).is_none()
    {
        return Err(alloc::format!("{o} is not installed"));
    }
    if plan.is_empty() {
        println!("milk: everything is up to date");
        return Ok(());
    }
    for (n, a, b) in &plan {
        println!("milk: upgrade {n} {a} -> {b}");
    }
    let (root, apps) = open_clone("upgrade")?;
    let mut done = Vec::new();
    for (n, _, v) in &plan {
        let auto = is_auto(&apps, n);
        let spec = alloc::format!("{n}={v}");
        if let Err(e) = install_tx(&apps, &spec, None, auto, true, &mut Vec::new(), &mut done) {
            close_clone("upgrade");
            audit(
                "pkg.upgrade_failed",
                &alloc::format!("name={n} version={v} reason={e} clone=discarded"),
            );
            return Err(alloc::format!(
                "upgrade of {n} failed ({e}); clone discarded, nothing changed"
            ));
        }
    }
    let summary: Vec<String> = plan
        .iter()
        .map(|(n, a, b)| alloc::format!("{n}:{a}->{b}"))
        .collect();
    audit(
        "pkg.upgrade",
        &alloc::format!(
            "packages={} clone=try-upgrade at={root} installed={}",
            summary.join(","),
            done.len()
        ),
    );
    println!(
        "milk: upgraded {} package(s) in clone try-upgrade; the live system is untouched.",
        plan.len()
    );
    if yes {
        return cmd_keep("upgrade");
    }
    for (n, _, _) in &plan {
        if let Some(m) =
            text(&alloc::format!("{apps}/{n}/manifest")).and_then(|t| Manifest::parse(&t))
            && let Some(e) = &m.exec
        {
            println!("milk: try it:   {apps}/{n}/{e}");
        }
    }
    println!("milk: then:     milk keep upgrade  or  milk discard upgrade");
    Ok(())
}

fn cmd_keep(name: &str) -> Res<()> {
    let c = clone_name(name);
    clone_path(&c).ok_or_else(|| alloc::format!("not trying {name} (no clone {c})"))?;
    milkfs("promote", &c)?;
    // The origin snapshot is not needed any more (before-<clone> holds the undo).
    let _ = milkfs("delete", &snap_name(name));
    if name == "upgrade" {
        cmd_relink(false);
        let list: Vec<String> = installed(APPS)
            .into_iter()
            .map(|(n, m)| alloc::format!("{n}:{}", m.version))
            .collect();
        audit(
            "pkg.keep",
            &alloc::format!(
                "name=upgrade clone={c} now={} undo=rollback:before-{c}",
                list.join(",")
            ),
        );
        println!(
            "milk: kept the upgrade: clone {c} is now the live system (undo: echo before-{c} > {MILKFS}/rollback)"
        );
        return Ok(());
    }
    // The promoted clone is the live volume now; /Applications shows it.
    let man = text(&alloc::format!("{APPS}/{name}/manifest"))
        .and_then(|t| Manifest::parse(&t))
        .ok_or_else(|| alloc::format!("{name} not found in {APPS} after promoting {c}"))?;
    cmd_relink(true);
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
    if name == "upgrade" {
        println!("milk: discarded clone {c}; the live system never saw the upgrade");
    } else {
        println!("milk: discarded clone {c}; the live system never saw {name}");
    }
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
        "usage: milk [--insecure] update | search [WORD] | info NAME | install NAME[=VER] | list\n       milk remove [--force] NAME | autoremove | upgrade [NAME] [-y]\n       milk [--insecure] try NAME | keep NAME|upgrade | discard NAME|upgrade | relink [-q] | keys | selftest"
    );
    2
}

fn main(args: &rt::Args) -> i32 {
    let mut a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    if let Some(i) = a.iter().position(|x| *x == "--insecure") {
        a.remove(i);
        INSECURE.store(true, core::sync::atomic::Ordering::Relaxed);
        eprintln!("milk: --insecure: unsigned or badly signed files will be accepted");
    }
    let r = match a.as_slice() {
        ["selftest"] => cmd_selftest(),
        ["keys"] => {
            cmd_keys();
            Ok(())
        }
        ["update"] => cmd_update(),
        ["search"] => cmd_search(None),
        ["search", w] => cmd_search(Some(w)),
        ["info", n] => cmd_info(n),
        ["install", n] => cmd_install(n),
        ["remove", n] => cmd_remove(n, false),
        ["remove", "--force", n] => cmd_remove(n, true),
        ["autoremove"] => cmd_autoremove(),
        ["upgrade"] => cmd_upgrade(None, false),
        ["upgrade", "-y"] => cmd_upgrade(None, true),
        ["upgrade", n] => cmd_upgrade(Some(n), false),
        ["upgrade", n, "-y"] | ["upgrade", "-y", n] => cmd_upgrade(Some(n), true),
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
