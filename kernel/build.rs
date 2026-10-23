use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.S");
    let repo = Path::new(&dir).parent().unwrap();
    // The one version number (<repo>/VERSION): env!("MILKOS_VERSION") in the kernel, and
    // @VERSION@ in rootfs text files (motd, System/Version.txt) is replaced with it.
    let version = read_version(repo);
    println!("cargo:rustc-env=MILKOS_VERSION={version}");
    println!("cargo:rerun-if-changed={}", repo.join("VERSION").display());
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

fn read_version(repo: &Path) -> String {
    let v = fs::read_to_string(repo.join("VERSION")).expect("read <repo>/VERSION");
    let v = v.trim();
    assert!(
        !v.is_empty() && v.split('.').all(|p| p.parse::<u32>().is_ok()),
        "VERSION must be like 0.1.0, got {v:?}"
    );
    v.to_string()
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
    // shared crypto and milkweb crates.
    println!("cargo:rerun-if-changed={}", user.display());
    println!("cargo:rerun-if-changed={}", repo.join("crypto").display());
    println!("cargo:rerun-if-changed={}", repo.join("milkweb").display());
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
