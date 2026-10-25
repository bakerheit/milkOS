use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The linker script, with the image's physical load address for the board being built:
/// qemu virt loads a kernel at RAM + 0x80000 = 0x4008_0000, the Raspberry Pi firmware (and
/// qemu raspi4b) at 0x80000 (feature `raspi`; kernel/src/platform.rs has the rest of the board).
fn link_script(dir: &str) {
    let raspi = std::env::var_os("CARGO_FEATURE_RASPI").is_some();
    let phys = if raspi { "0x80000" } else { "0x40080000" };
    let text = fs::read_to_string(format!("{dir}/linker.ld")).expect("read linker.ld");
    let placeholder = "KERNEL_PHYS = 0x40080000;";
    assert!(
        text.contains(placeholder),
        "linker.ld lost its KERNEL_PHYS line"
    );
    let text = text.replace(placeholder, &format!("KERNEL_PHYS = {phys};"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("linker.ld");
    fs::write(&out, text).expect("write linker.ld");
    println!("cargo:rustc-link-arg=-T{}", out.display());
    println!("cargo:rerun-if-changed=linker.ld");
}

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    link_script(&dir);
    println!("cargo:rerun-if-changed=src/boot.S");
    let repo = Path::new(&dir).parent().unwrap();
    // The one version number (<repo>/VERSION), see docs/versioning.md:
    //   MILKOS_VERSION      what people see: `0.2.0` for a release build (MILKOS_RELEASE=1, set by
    //                       scripts/release.sh), `0.2.0-dev (abc1234)` for any other build, with
    //                       `+dirty` after the hash when the tree has changes, or `unknown` for the
    //                       hash when there is no git (a source tarball). uname, /proc/version, the
    //                       boot message, About, Settings, @VERSION@ in rootfs text files.
    //   MILKOS_VERSION_NUM  the plain number from VERSION.
    //   MILKOS_BUILD        `release`, `abc1234`, `abc1234+dirty` or `unknown`.
    let number = read_version(repo);
    let (version, build) = version_strings(repo, &number);
    println!("cargo:rustc-env=MILKOS_VERSION={version}");
    println!("cargo:rustc-env=MILKOS_VERSION_NUM={number}");
    println!("cargo:rustc-env=MILKOS_BUILD={build}");
    // The git commit the tree is at, also for a release build (the SD card log's boot banner carries it).
    let git_id = git_build_id(repo).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=MILKOS_GIT={git_id}");
    println!("cargo:rerun-if-changed={}", repo.join("VERSION").display());
    println!("cargo:rerun-if-env-changed=MILKOS_RELEASE");
    // User programs first: the initramfs packs them (initramfs.list).
    build_user(repo);
    build_initramfs(repo, &version);
    build_art(repo);
}

// ---- art ------------------------------------------------------------------------------
// docs/ui-concepts/art (icons, wallpapers, the splash) decoded from PNG into raw tables by
// scripts/art_embed.py (python3 stdlib only), included by src/art.rs. If that fails (no
// python3), the kernel builds with no art and draws its own icons and wallpapers.

fn build_art(repo: &Path) {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("art");
    let art = repo.join("docs/ui-concepts/art");
    let script = repo.join("scripts/art_embed.py");
    println!("cargo:rerun-if-changed={}", art.join("icons").display());
    println!(
        "cargo:rerun-if-changed={}",
        art.join("wallpapers").display()
    );
    println!("cargo:rerun-if-changed={}", art.join("splash").display());
    println!("cargo:rerun-if-changed={}", script.display());
    let ok = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(&art)
        .arg(&out)
        .output()
        .map(|o| {
            if !o.status.success() {
                println!(
                    "cargo:warning=art_embed.py failed, building without art: {}",
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .last()
                        .unwrap_or("")
                );
            }
            o.status.success()
        })
        .unwrap_or(false);
    if !ok {
        fs::create_dir_all(&out).unwrap();
        fs::write(
            out.join("art.rs"),
            "pub static ICONS: &[IconArt] = &[];\npub static IMAGES: &[ImageArt] = &[];\n",
        )
        .unwrap();
    }
}

/// `MAJOR.MINOR.PATCH` with an optional `-prerelease` (dot-separated identifiers of
/// `[0-9A-Za-z-]`, numeric ones without leading zeros), which is what cargo also takes.
fn is_semver(v: &str) -> bool {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let num = |p: &str| {
        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p == "0" || !p.starts_with('0'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| num(p)) {
        return false;
    }
    pre.is_none_or(|p| {
        p.split('.').all(|id| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!id.bytes().all(|b| b.is_ascii_digit()) || num(id))
        })
    })
}

fn read_version(repo: &Path) -> String {
    let v = fs::read_to_string(repo.join("VERSION")).expect("read <repo>/VERSION");
    let v = v.trim();
    assert!(
        is_semver(v),
        "VERSION must be like 0.2.0 or 0.3.0-rc.1, got {v:?}"
    );
    v.to_string()
}

/// (MILKOS_VERSION, MILKOS_BUILD) for `number`: see `main`.
fn version_strings(repo: &Path, number: &str) -> (String, String) {
    let release = std::env::var("MILKOS_RELEASE").is_ok_and(|v| v == "1");
    if release {
        return (number.to_string(), "release".to_string());
    }
    let build = git_build_id(repo).unwrap_or_else(|| "unknown".to_string());
    let version = format!("{number}-dev ({build})");
    // uname fields are 64 bytes; the longest user is "#1 milkOS <version> (aarch64, Rust)".
    assert!(version.len() <= 40, "version string too long: {version}");
    (version, build)
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    // GIT_OPTIONAL_LOCKS=0: `git status` must not rewrite the index (that would make cargo
    // think the inputs changed).
    let o = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// `abc1234` or `abc1234+dirty` for the checkout at `repo`; None without git or when `repo`
/// is not itself the top of a git checkout (a source tarball unpacked inside someone else's
/// repository must not report that repository's commit).
fn git_build_id(repo: &Path) -> Option<String> {
    let top = git(repo, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(&top).ok()? != fs::canonicalize(repo).ok()? {
        return None;
    }
    let hash = git(repo, &["rev-parse", "--short=7", "HEAD"])?;
    // Rebuild when the commit changes (the reflog of this checkout moves on every commit,
    // checkout and reset), and when kernel sources change: whether the tree
    // is dirty is decided when this script runs.
    for p in ["logs/HEAD", "HEAD"] {
        if let Some(path) = git(repo, &["rev-parse", "--git-path", p]) {
            let path = if Path::new(&path).is_absolute() {
                PathBuf::from(path)
            } else {
                repo.join(path)
            };
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    println!(
        "cargo:rerun-if-changed={}",
        repo.join("kernel/src").display()
    );
    let dirty = git(repo, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    Some(if dirty { format!("{hash}+dirty") } else { hash })
}

// ---- user programs ------------------------------------------------------------------
// <repo>/user is its own workspace (target, linker script). Build it with a nested
// `cargo build --release` into <repo>/target/user, then check that every program
// (user/<name>/src/main.rs) is listed in initramfs.list, so none silently goes missing.

const USER_BIN_DIR: &str = "target/user/aarch64-unknown-none-softfloat/release";

fn build_user(repo: &Path) {
    let user = repo.join("user");
    if !user.join("Cargo.toml").exists() {
        return;
    }
    // A directory: cargo rescans everything under it for changes. user/rt also uses the
    // shared crypto, milkweb and tls crates (and tls compiles in assets/ca).
    println!("cargo:rerun-if-changed={}", user.display());
    println!("cargo:rerun-if-changed={}", repo.join("crypto").display());
    println!("cargo:rerun-if-changed={}", repo.join("milkweb").display());
    println!("cargo:rerun-if-changed={}", repo.join("tls").display());
    println!(
        "cargo:rerun-if-changed={}",
        repo.join("assets/ca").display()
    );
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&user)
        .args(["build", "--release", "--target-dir"])
        .arg(repo.join("target/user"));
    // Don't leak this (kernel) build's settings into the nested one.
    for (k, _) in std::env::vars() {
        if (k.starts_with("CARGO_") && k != "CARGO_HOME")
            || k == "RUSTFLAGS"
            || k == "RUSTC_WORKSPACE_WRAPPER"
            || k == "RUSTC_WRAPPER"
        {
            cmd.env_remove(&k);
        }
    }
    let o = cmd.output().expect("failed to run cargo for user/");
    if !o.status.success() {
        panic!(
            "building user/ failed:\n{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let list = fs::read_to_string(repo.join("initramfs.list")).unwrap_or_default();
    // Programs: user/<crate>/src/main.rs is binary <crate>; user/<crate>/src/bin/<x>.rs is <x>.
    let mut programs = Vec::new();
    for e in fs::read_dir(&user).unwrap().filter_map(|e| e.ok()) {
        if e.path().join("src/main.rs").exists() {
            programs.push(e.file_name().to_string_lossy().into_owned());
        }
        if let Ok(bins) = fs::read_dir(e.path().join("src/bin")) {
            for b in bins.filter_map(|b| b.ok()) {
                let p = b.path();
                if p.extension().is_some_and(|x| x == "rs") {
                    programs.push(p.file_stem().unwrap().to_string_lossy().into_owned());
                }
            }
        }
    }
    programs.sort();
    for name in programs {
        let host = format!("{USER_BIN_DIR}/{name}");
        assert!(
            repo.join(&host).exists(),
            "user program {name} was not built ({host})"
        );
        assert!(
            list.lines()
                .any(|l| l.split_whitespace().nth(1) == Some(host.as_str())),
            "user program {name} is not in initramfs.list; add (a command; an app goes in /Apps/<name>/, a service in /System/Services, a test or developer tool in /System/Developer):\n/Commands/{name}  {host}  755"
        );
    }
}

// ---- initramfs ----------------------------------------------------------------------
// Packs <repo>/rootfs/ plus the extra files named in <repo>/initramfs.list into a cpio
// "newc" archive at $OUT_DIR/initramfs.cpio, which the kernel embeds with include_bytes!
// and unpacks into the root tmpfs at boot. Entries are sorted, mtimes are 0, uid/gid 0,
// so the archive is reproducible. See docs/filesystem.md.

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;

struct Cpio {
    out: Vec<u8>,
    ino: u32,
}

impl Cpio {
    fn entry(&mut self, name: &str, mode: u32, data: &[u8]) {
        self.ino += 1;
        let name_size = name.len() + 1;
        let header = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            self.ino,
            mode,
            0, // uid
            0, // gid
            if mode & S_IFDIR != 0 { 2 } else { 1 },
            0, // mtime
            data.len(),
            0,
            0, // dev major/minor
            0,
            0, // rdev major/minor
            name_size,
            0, // check
        );
        self.out.extend_from_slice(header.as_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.push(0);
        self.pad();
        self.out.extend_from_slice(data);
        self.pad();
    }

    fn pad(&mut self) {
        while self.out.len() % 4 != 0 {
            self.out.push(0);
        }
    }
}

/// Every path under `root`, sorted, relative, depth-first (parents before children).
fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) {
    let mut names: Vec<_> = fs::read_dir(root.join(rel))
        .unwrap_or_else(|e| panic!("initramfs: read {}: {e}", root.join(rel).display()))
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    for n in names {
        if n == ".DS_Store" || n == ".gitkeep" {
            continue;
        }
        let r = rel.join(&n);
        out.push(r.clone());
        if fs::symlink_metadata(root.join(&r)).unwrap().is_dir() {
            walk(root, &r, out);
        }
    }
}

/// Make sure every parent directory of `name` has an entry.
fn ensure_parents(cpio: &mut Cpio, seen: &mut Vec<String>, name: &str) {
    let mut acc = String::new();
    let parts: Vec<&str> = name.split('/').collect();
    for p in &parts[..parts.len() - 1] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(p);
        if !seen.contains(&acc) {
            seen.push(acc.clone());
            cpio.entry(&acc, S_IFDIR | 0o755, &[]);
        }
    }
}

fn build_initramfs(repo: &Path, version: &str) {
    let rootfs = repo.join("rootfs");
    let list = repo.join("initramfs.list");
    println!("cargo:rerun-if-changed={}", rootfs.display());
    println!("cargo:rerun-if-changed={}", list.display());

    let mut cpio = Cpio {
        out: Vec::new(),
        ino: 0,
    };
    let mut seen: Vec<String> = Vec::new();

    if rootfs.is_dir() {
        let mut paths = Vec::new();
        walk(&rootfs, Path::new(""), &mut paths);
        for rel in paths {
            let full = rootfs.join(&rel);
            let name = rel
                .to_str()
                .expect("initramfs: non-UTF-8 path")
                .replace('\\', "/");
            let meta = fs::symlink_metadata(&full).unwrap();
            let perm = meta.permissions().mode() & 0o7777;
            seen.push(name.clone());
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&full).unwrap();
                cpio.entry(&name, S_IFLNK | 0o777, target.to_str().unwrap().as_bytes());
            } else if meta.is_dir() {
                cpio.entry(&name, S_IFDIR | perm, &[]);
            } else {
                let mut data = fs::read(&full).unwrap();
                if let Ok(text) = std::str::from_utf8(&data)
                    && text.contains("@VERSION@")
                {
                    data = text.replace("@VERSION@", version).into_bytes();
                }
                cpio.entry(&name, S_IFREG | perm, &data);
            }
        }
    }

    if let Ok(text) = fs::read_to_string(&list) {
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 2 || f.len() > 3 {
                panic!(
                    "initramfs.list:{}: want `<dest> <source> [mode]`",
                    lineno + 1
                );
            }
            let dest = f[0].trim_start_matches('/');
            let src = repo.join(f[1]);
            println!("cargo:rerun-if-changed={}", src.display());
            let data = match fs::read(&src) {
                Ok(d) => d,
                Err(e) => {
                    println!(
                        "cargo:warning=initramfs.list:{}: skipping {} ({e})",
                        lineno + 1,
                        src.display()
                    );
                    continue;
                }
            };
            let mode = match f.get(2) {
                Some(m) => u32::from_str_radix(m, 8)
                    .unwrap_or_else(|_| panic!("initramfs.list:{}: bad mode {m}", lineno + 1)),
                None => fs::metadata(&src).unwrap().permissions().mode() & 0o7777,
            };
            ensure_parents(&mut cpio, &mut seen, dest);
            cpio.entry(dest, S_IFREG | mode, &data);
        }
    }

    cpio.entry("TRAILER!!!", 0, &[]);
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("initramfs.cpio");
    fs::File::create(&out)
        .and_then(|mut f| f.write_all(&cpio.out))
        .expect("initramfs: write archive");
}
