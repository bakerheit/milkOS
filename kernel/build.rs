use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=src/boot.S");
    build_initramfs(Path::new(&dir).parent().unwrap());
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

fn build_initramfs(repo: &Path) {
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
                cpio.entry(&name, S_IFREG | perm, &fs::read(&full).unwrap());
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
