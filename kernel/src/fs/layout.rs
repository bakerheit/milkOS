//! The folder layout (docs/filesystem.md, "The tree"), in plain English:
//!
//! | folder | what |
//! |---|---|
//! | `/Apps` | apps you open: the ones that ship with milkOS and the ones `apps` installs |
//! | `/Commands` | what you type in Terminal (`PATH`) |
//! | `/Users/<name>` | homes: `/Users/milk` (persistent), `/Users/root` |
//! | `/Volumes` | disks, USB sticks, the Mac's shared folder |
//! | `/System` | the OS's own files: `Services` (and their `Sockets`), `Startup`, `Settings`, `Kernel` (kernel controls), `Developer` (test tools), `Fonts`, `Sounds`, `Music` |
//! | `/dev`, `/proc`, `/tmp` | Unix plumbing Linux and POSIX programs expect; hidden in listings |
//!
//! Two jobs live here, both run while a milkfs volume becomes the primary one (at boot, or
//! when a login unlocks it), before its folders are bound over the RAM copies:
//!
//! * [`migrate`]: a volume made before this layout (`home/milk`, `Applications`,
//!   `etc/milk/drop`, `etc/milk/net`) is moved over once, by renames inside the volume.
//!   (`/run` and `/sys`, now `/System/Services/Sockets` and `/System/Kernel`, need nothing:
//!   both are made at boot, never on a disk, and nothing a volume keeps names them.)
//! * [`sync_builtin_apps`]: the apps that ship in the initramfs (unpacked into the RAM
//!   `/Apps`) are copied into the volume's `Apps` folder, which is then bound onto `/Apps`,
//!   so built-in and installed apps sit side by side in one folder. A copy is only
//!   rewritten when it differs (a new milkOS version), and a built-in app that a newer
//!   milkOS no longer ships is removed.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::node::InodeType;
use crate::fs::ops;
use crate::kprintln;
use crate::sync::SpinLock;

pub const APPS: &str = "/Apps";
pub const COMMANDS: &str = "/Commands";
pub const USERS: &str = "/Users";
pub const SETTINGS: &str = "/System/Settings";
pub const SERVICES: &str = "/System/Services";
/// The services' AF_UNIX sockets (`milkd.sock`, `drop.sock`), made at boot (tmpfs).
pub const SOCKETS: &str = "/System/Services/Sockets";
pub const STARTUP: &str = "/System/Startup";
/// sysfs: the kernel's controls and the hardware it was handed (`fs/milkfs/*`, `devices/`).
pub const KERNEL: &str = "/System/Kernel";
/// Test harnesses and developer tools (`utest`, `fault`, `linux-hello`, ...), not on PATH.
pub const DEVELOPER: &str = "/System/Developer";

/// Where the volume remembers which built-in apps it holds copies of.
const BUILTIN_LIST: &str = "/.milkos/builtin-apps";

/// The apps in the initramfs (folder names under `/Apps`), recorded at boot.
static BUILTIN: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

/// Remember which apps the initramfs brought (call once, right after it is unpacked and
/// before anything is bound onto `/Apps`).
pub fn record_builtin_apps() {
    let mut names: Vec<String> = ops::readdir(APPS)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.kind == InodeType::Dir && !e.name.starts_with('.'))
        .map(|e| e.name)
        .collect();
    names.sort();
    *BUILTIN.lock() = names;
}

/// Does `name` (a folder under `/Apps`) ship with milkOS? (The desktop runs those
/// unsandboxed.) It must be a name this initramfs brought, with no `.origin` (a folder
/// `apps` installed is never a built-in app, whatever its name), and a root-owned folder
/// nobody else can write: `/Apps` is admin-writable, so an admin could swap a folder, but
/// only for one of their own, which then runs like any installed app.
pub fn is_builtin_app(name: &str) -> bool {
    let dir = format!("{APPS}/{name}");
    BUILTIN.lock().iter().any(|n| n == name)
        && ops::lstat(&format!("{dir}/.origin")).is_err()
        && ops::lstat(&dir)
            .is_ok_and(|s| s.kind == InodeType::Dir && s.uid == 0 && s.perm & 0o022 == 0)
}

fn exists(p: &str) -> bool {
    ops::lstat(p).is_ok()
}

fn is_dir(p: &str) -> bool {
    ops::lstat(p).is_ok_and(|s| s.kind == InodeType::Dir)
}

/// Move folder `from` to `to` inside one volume: a rename, or (when `to` is already
/// there) every entry `to` doesn't have yet. `from` goes if it ends up empty.
fn move_dir(from: &str, to: &str, moved: &mut Vec<String>) {
    if !is_dir(from) {
        return;
    }
    if !exists(to) {
        if let Some((parent, _)) = to.rsplit_once('/') {
            let _ = ops::mkdir_all(parent, 0o755);
        }
        match ops::rename(from, to) {
            Ok(()) => moved.push(format!("{from} -> {to}")),
            Err(e) => kprintln!("layout: can't move {from} to {to}: {e}"),
        }
        return;
    }
    for e in ops::readdir(from).unwrap_or_default() {
        let (f, t) = (format!("{from}/{}", e.name), format!("{to}/{}", e.name));
        if exists(&t) {
            kprintln!("layout: {t} is already there; leaving {f}");
            continue;
        }
        match ops::rename(&f, &t) {
            Ok(()) => moved.push(format!("{f} -> {t}")),
            Err(err) => kprintln!("layout: can't move {f} to {t}: {err}"),
        }
    }
    let _ = ops::rmdir(from);
}

/// Move a volume made before this layout over to it (see the module docs). Idempotent:
/// nothing happens on a volume that has none of the old folders. `vol` is where it is
/// mounted (`/Volumes/Milk`).
pub fn migrate(vol: &str) {
    let mut moved = Vec::new();
    let home_was = is_dir(&format!("{vol}/home/milk")) && !exists(&format!("{vol}/Users/milk"));
    move_dir(&format!("{vol}/home"), &format!("{vol}{USERS}"), &mut moved);
    if home_was && is_dir(&format!("{vol}/Users/milk")) {
        // The web server's pages moved into the home folder: give the moved home the
        // shipped ~/Sites (the RAM copy, not yet covered by the bind), as a new disk gets.
        let sites = format!("{vol}/Users/milk/Sites");
        if !exists(&sites) && is_dir("/Users/milk/Sites") {
            match ops::copy_tree("/Users/milk/Sites", &sites) {
                Ok(()) => moved.push(format!("(new) {sites}")),
                Err(e) => kprintln!("layout: can't create {sites}: {e}"),
            }
        }
        // The search index holds the old paths; it is a cache, rebuilt from the files.
        let index = format!("{vol}{}", crate::fs::search::INDEX_PATH);
        let _ = ops::unlink(&index);
    }
    move_dir(
        &format!("{vol}/Applications"),
        &format!("{vol}{APPS}"),
        &mut moved,
    );
    for d in ["drop", "net"] {
        move_dir(
            &format!("{vol}/etc/milk/{d}"),
            &format!("{vol}{SETTINGS}/milk/{d}"),
            &mut moved,
        );
    }
    let _ = ops::rmdir(&format!("{vol}/etc/milk"));
    let _ = ops::rmdir(&format!("{vol}/etc"));
    if !moved.is_empty() {
        kprintln!(
            "layout: moved {vol} to the new folder layout: {}",
            moved.join(", ")
        );
        crate::audit::record(
            "fs.layout.migrate",
            &format!("volume={vol} moved={}", moved.len()),
        );
    }
}

/// Are the trees at `a` and `b` the same (names, kinds, file bytes, symlink targets)?
fn same_tree(a: &str, b: &str) -> bool {
    let (Ok(mut ea), Ok(mut eb)) = (ops::readdir(a), ops::readdir(b)) else {
        return false;
    };
    if ea.len() != eb.len() {
        return false;
    }
    ea.sort_by(|x, y| x.name.cmp(&y.name));
    eb.sort_by(|x, y| x.name.cmp(&y.name));
    ea.iter().zip(&eb).all(|(x, y)| {
        if x.name != y.name || x.kind != y.kind {
            return false;
        }
        let (pa, pb) = (format!("{a}/{}", x.name), format!("{b}/{}", y.name));
        match x.kind {
            InodeType::Dir => same_tree(&pa, &pb),
            InodeType::Symlink => ops::readlink(&pa).ok() == ops::readlink(&pb).ok(),
            InodeType::File => {
                ops::lstat(&pa).map(|s| (s.size, s.perm)).ok()
                    == ops::lstat(&pb).map(|s| (s.size, s.perm)).ok()
                    && ops::read_file(&pa).ok() == ops::read_file(&pb).ok()
            }
            _ => true,
        }
    })
}

fn rm_tree(path: &str) {
    match ops::lstat(path) {
        Ok(st) if st.kind == InodeType::Dir => {
            for e in ops::readdir(path).unwrap_or_default() {
                rm_tree(&format!("{path}/{}", e.name));
            }
            let _ = ops::rmdir(path);
        }
        Ok(_) => {
            let _ = ops::unlink(path);
        }
        Err(_) => {}
    }
}

/// Put the built-in apps (the RAM `/Apps`, still uncovered) into `on_disk` (the volume's
/// `Apps` folder, about to be bound onto `/Apps`). `vol`: the volume's mount path.
pub fn sync_builtin_apps(vol: &str, on_disk: &str) {
    let builtin = BUILTIN.lock().clone();
    let list = format!("{vol}{BUILTIN_LIST}");
    let before: Vec<String> = ops::read_to_string(&list)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    let mut copied = 0;
    for name in &builtin {
        let (src, dst) = (format!("{APPS}/{name}"), format!("{on_disk}/{name}"));
        if exists(&format!("{dst}/.origin")) {
            kprintln!("layout: an installed package is called {name}; the built-in app stays out");
            continue;
        }
        if same_tree(&src, &dst) {
            continue;
        }
        rm_tree(&dst);
        match ops::copy_tree(&src, &dst) {
            Ok(()) => copied += 1,
            Err(e) => kprintln!("layout: can't copy {src} to {dst}: {e}"),
        }
    }
    // Built-in apps an older milkOS put there that this one doesn't ship.
    for name in before.iter().filter(|n| !builtin.contains(n)) {
        let dst = format!("{on_disk}/{name}");
        if !name.is_empty() && !name.contains('/') && !exists(&format!("{dst}/.origin")) {
            rm_tree(&dst);
        }
    }
    if before != builtin {
        if let Some((dir, _)) = list.rsplit_once('/') {
            let _ = ops::mkdir_all(dir, 0o755);
        }
        let mut text = builtin.join("\n");
        text.push('\n');
        if let Err(e) = ops::write_file(&list, text.as_bytes()) {
            kprintln!("layout: can't write {list}: {e}");
        }
    }
    if copied > 0 {
        kprintln!("layout: {copied} built-in app(s) copied into {on_disk}");
    }
}
