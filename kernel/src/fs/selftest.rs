//! Boot self-test for the VFS, tmpfs, initramfs, devfs and procfs/sysfs. Panics on failure
//! (like `memtest`), then prints `/System/Settings/motd`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::abi::*;
use super::errno::Errno;
use super::node::{DT_DIR, InodeType, makedev};
use super::{ops, path};
use crate::{kprintln, pmm};

struct T {
    checks: u32,
    failed: u32,
}

impl T {
    fn ok(&mut self, cond: bool, what: &str) {
        self.checks += 1;
        if !cond {
            self.failed += 1;
            kprintln!("fs selftest: FAIL: {what}");
        }
    }

    fn err<V>(&mut self, r: Result<V, Errno>, want: Errno, what: &str) {
        self.checks += 1;
        match r {
            Err(e) if e == want => {}
            Err(e) => {
                self.failed += 1;
                kprintln!("fs selftest: FAIL: {what}: got {e}, want {want}");
            }
            Ok(_) => {
                self.failed += 1;
                kprintln!("fs selftest: FAIL: {what}: succeeded, want {want}");
            }
        }
    }

    /// Unwrap or record a failure and return `None`.
    fn must<V>(&mut self, r: Result<V, Errno>, what: &str) -> Option<V> {
        self.checks += 1;
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.failed += 1;
                kprintln!("fs selftest: FAIL: {what}: {e}");
                None
            }
        }
    }
}

fn read_str(path: &str) -> String {
    ops::read_to_string(path).unwrap_or_default()
}

/// Parse a getdents64 buffer into names (and check record sanity).
fn parse_dirents(buf: &[u8]) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + DIRENT64_HEADER <= buf.len() {
        let reclen = u16::from_ne_bytes([buf[off + 16], buf[off + 17]]) as usize;
        if reclen == 0 || reclen % 8 != 0 || off + reclen > buf.len() {
            break;
        }
        let name_bytes = &buf[off + 19..off + reclen];
        let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(0);
        out.push((
            String::from_utf8_lossy(&name_bytes[..end]).into_owned(),
            buf[off + 18],
        ));
        off += reclen;
    }
    out
}

fn tmpfs_basics(t: &mut T) {
    t.ok(ops::mkdir("/tmp/t", 0o755).is_ok(), "mkdir /tmp/t");
    let Some(f) = t.must(
        ops::open("/tmp/t/a.txt", O_RDWR | O_CREAT | O_EXCL, 0o644),
        "create a.txt",
    ) else {
        return;
    };
    t.ok(f.write(b"hello world").ok() == Some(11), "write 11 bytes");
    t.ok(f.seek(0, SEEK_SET).ok() == Some(0), "lseek 0");
    let mut buf = [0u8; 32];
    t.ok(f.read(&mut buf).ok() == Some(11), "read back 11 bytes");
    t.ok(&buf[..11] == b"hello world", "content matches");
    t.ok(f.read(&mut buf).ok() == Some(0), "read at EOF returns 0");
    let st = f.stat();
    t.ok(
        st.size == 11 && st.kind == InodeType::File && st.perm == 0o644,
        "stat size/type/perm",
    );
    t.ok(
        f.pwrite(b"W", 6).ok() == Some(1) && read_str("/tmp/t/a.txt") == "hello World",
        "pwrite in place",
    );

    // O_APPEND goes to the end whatever the offset.
    if let Some(a) = t.must(
        ops::open("/tmp/t/a.txt", O_WRONLY | O_APPEND, 0),
        "open O_APPEND",
    ) {
        let _ = a.seek(0, SEEK_SET);
        t.ok(a.write(b"!").ok() == Some(1), "append write");
        t.ok(
            read_str("/tmp/t/a.txt") == "hello World!",
            "append landed at end",
        );
    }
    t.ok(f.seek(0, SEEK_END).ok() == Some(12), "SEEK_END");
    t.err(f.seek(-100, SEEK_CUR), Errno::EINVAL, "seek before 0");

    // O_TRUNC.
    t.ok(
        ops::open("/tmp/t/a.txt", O_WRONLY | O_TRUNC, 0).is_ok()
            && ops::stat("/tmp/t/a.txt").map(|s| s.size).ok() == Some(0),
        "O_TRUNC empties the file",
    );
    t.ok(
        ops::write_file("/tmp/t/a.txt", b"hello world").is_ok(),
        "write_file",
    );

    // Linux struct stat layout.
    let ls = st.to_linux();
    let b = ls.as_bytes();
    t.ok(
        u32::from_ne_bytes(b[16..20].try_into().unwrap()) == 0o100644
            && i64::from_ne_bytes(b[48..56].try_into().unwrap()) == 11,
        "struct stat: st_mode @16, st_size @48",
    );
}

fn tmpfs_big_and_sparse(t: &mut T) {
    let free0 = pmm::stats().free_frames;
    // 300 KiB with a position-dependent pattern, written in odd-sized chunks.
    let len = 300 * 1024 + 123;
    let data: Vec<u8> = (0..len).map(|i| (i * 7 + i / 4096) as u8).collect();
    let Some(f) = t.must(
        ops::open("/tmp/t/big", O_RDWR | O_CREAT, 0o600),
        "create big",
    ) else {
        return;
    };
    for chunk in data.chunks(5000) {
        let _ = f.write(chunk);
    }
    t.ok(f.stat().size == len as u64, "big file size");
    t.ok(
        ops::read_file("/tmp/t/big").ok().as_deref() == Some(&data[..]),
        "big file reads back",
    );
    // Truncate down and grow again: the old tail must read as zeros.
    t.ok(f.truncate(5000).is_ok(), "ftruncate 5000");
    t.ok(f.truncate(9000).is_ok(), "ftruncate 9000");
    let mut back = vec![0u8; 9000];
    t.ok(
        f.pread(&mut back, 0).ok() == Some(9000),
        "pread after regrow",
    );
    t.ok(
        back[..5000] == data[..5000] && back[5000..].iter().all(|&b| b == 0),
        "truncated tail reads as zeros",
    );

    // Sparse: one byte at 1 MiB allocates one page, the hole reads as zeros.
    let Some(s) = t.must(
        ops::open("/tmp/t/sparse", O_RDWR | O_CREAT, 0o600),
        "create sparse",
    ) else {
        return;
    };
    t.ok(s.pwrite(b"x", 1 << 20).ok() == Some(1), "write at 1 MiB");
    let st = s.stat();
    t.ok(
        st.size == (1 << 20) + 1 && st.blocks == 8,
        "sparse: size 1 MiB+1, 8 blocks",
    );
    let mut hole = [1u8; 64];
    t.ok(
        s.pread(&mut hole, 4096 * 3).ok() == Some(64) && hole.iter().all(|&b| b == 0),
        "hole reads as zeros",
    );
    drop(s);
    drop(f);
    t.ok(
        ops::unlink("/tmp/t/big").is_ok() && ops::unlink("/tmp/t/sparse").is_ok(),
        "unlink big + sparse",
    );
    t.ok(
        pmm::stats().free_frames == free0,
        "all data frames returned to the PMM",
    );
}

/// 9000 bytes standing in for a file in the kernel image.
static ROM: [u8; 9000] = {
    let mut a = [0u8; 9000];
    let mut i = 0;
    while i < a.len() {
        a[i] = (i * 7 + i / 4096) as u8;
        i += 1;
    }
    a
};

/// Files whose data is the kernel image's (the initramfs): read in place, copied on the
/// first write or growing truncate, never counted as tmpfs pages until then.
fn tmpfs_rom(t: &mut T) {
    use super::tmpfs::TmpNode;
    let files = || pmm::stats().of(pmm::Use::Files);
    let rom_file = |path: &str| -> Option<super::file::OpenFile> {
        let f = ops::open(path, O_RDWR | O_CREAT | O_TRUNC, 0o644).ok()?;
        super::node::downcast::<TmpNode>(f.inode())?
            .set_rom(&ROM)
            .ok()?;
        Some(f)
    };
    let is_rom = |path: &str| {
        ops::lookup(path)
            .ok()
            .and_then(|i| super::node::downcast::<TmpNode>(&i).map(|n| n.is_rom()))
            == Some(true)
    };
    let f0 = files();
    let Some(f) = t.must(
        rom_file("/tmp/t/rom").ok_or(Errno::EIO),
        "a file over static bytes",
    ) else {
        return;
    };
    t.ok(
        f.stat().size == 9000 && files() == f0 && is_rom("/tmp/t/rom"),
        "rom file: 9000 bytes, no pages of its own",
    );
    t.ok(
        ops::read_file("/tmp/t/rom").ok().as_deref() == Some(&ROM[..]),
        "rom file reads its bytes",
    );
    let mut mid = [0u8; 100];
    t.ok(
        f.pread(&mut mid, 4050).ok() == Some(100) && mid[..] == ROM[4050..4150],
        "rom file: a read across a page boundary",
    );
    // A hard link shares it; the first write copies, the static bytes stay as they were.
    t.ok(
        ops::link("/tmp/t/rom", "/tmp/t/rom-link").is_ok(),
        "link rom file",
    );
    let before = ROM[5000];
    t.ok(
        f.pwrite(b"Z", 5000).ok() == Some(1),
        "write into a rom file",
    );
    let back = ops::read_file("/tmp/t/rom-link").unwrap_or_default();
    t.ok(
        back.len() == 9000
            && back[5000] == b'Z'
            && back[..5000] == ROM[..5000]
            && back[5001..] == ROM[5001..],
        "after the write: own copy with the change, seen through the link",
    );
    t.ok(
        ROM[5000] == before && files() == f0 + 3 && !is_rom("/tmp/t/rom"),
        "copy-on-write: 3 pages of its own, the original untouched",
    );
    // Truncates: shrinking stays in place, growing copies, to 0 drops the view.
    let Some(g) = t.must(rom_file("/tmp/t/rom2").ok_or(Errno::EIO), "second rom file") else {
        return;
    };
    t.ok(
        g.truncate(100).is_ok() && is_rom("/tmp/t/rom2") && files() == f0 + 3,
        "shrinking a rom file copies nothing",
    );
    t.ok(
        ops::read_file("/tmp/t/rom2").ok().as_deref() == Some(&ROM[..100]),
        "shrunk rom file reads its first 100 bytes",
    );
    t.ok(
        g.truncate(200).is_ok() && !is_rom("/tmp/t/rom2") && files() == f0 + 4,
        "growing a rom file gives it a page",
    );
    let grown = ops::read_file("/tmp/t/rom2").unwrap_or_default();
    t.ok(
        grown.len() == 200 && grown[..100] == ROM[..100] && grown[100..].iter().all(|&b| b == 0),
        "grown rom file: its bytes, then zeros",
    );
    let Some(h) = t.must(rom_file("/tmp/t/rom3").ok_or(Errno::EIO), "third rom file") else {
        return;
    };
    t.ok(
        h.truncate(0).is_ok() && h.stat().size == 0 && files() == f0 + 4,
        "truncating a rom file to 0 copies nothing",
    );
    t.ok(
        h.write(b"hi").ok() == Some(2)
            && ops::read_file("/tmp/t/rom3").ok().as_deref() == Some(&b"hi"[..]),
        "then it is an ordinary file",
    );
    // O_TRUNC over a rom file (how a program replaces one) copies nothing either.
    drop(rom_file("/tmp/t/rom4"));
    let f1 = files();
    t.ok(
        ops::write_file("/tmp/t/rom4", b"new").is_ok()
            && ops::read_file("/tmp/t/rom4").ok().as_deref() == Some(&b"new"[..])
            && files() == f1 + 1,
        "replacing a rom file (O_TRUNC) writes only the new data",
    );
    drop((f, g, h));
    for p in [
        "/tmp/t/rom",
        "/tmp/t/rom-link",
        "/tmp/t/rom2",
        "/tmp/t/rom3",
        "/tmp/t/rom4",
    ] {
        let _ = ops::unlink(p);
    }
    t.ok(files() == f0, "rom test files gone, their pages freed");
    // The built-in files are in the image, not copied (until written).
    t.ok(
        is_rom("/System/Version.txt") && is_rom("/Commands/text"),
        "built-in files read from the kernel image",
    );
    t.ok(
        pmm::stats().of(pmm::Use::Files) * 4096 < super::initramfs::archive_len() / 4,
        "the built-in files take no tmpfs pages",
    );
}

fn tmpfs_namespace(t: &mut T) {
    // rename a file
    t.ok(
        ops::rename("/tmp/t/a.txt", "/tmp/t/b.txt").is_ok(),
        "rename a.txt -> b.txt",
    );
    t.err(ops::stat("/tmp/t/a.txt"), Errno::ENOENT, "old name gone");
    t.ok(
        read_str("/tmp/t/b.txt") == "hello world",
        "content survives rename",
    );

    // directories
    t.ok(
        ops::mkdir("/tmp/t/d1", 0o755).is_ok()
            && ops::mkdir("/tmp/t/d2", 0o755).is_ok()
            && ops::write_file("/tmp/t/d1/f", b"in d1").is_ok(),
        "mkdir d1 d2, file in d1",
    );
    t.ok(
        ops::rename("/tmp/t/d1", "/tmp/t/d2/inner").is_ok(),
        "rename dir across dirs",
    );
    t.ok(
        read_str("/tmp/t/d2/inner/f") == "in d1",
        "file follows its dir",
    );
    t.ok(
        read_str("/tmp/t/d2/inner/../../b.txt") == "hello world",
        ".. after rename points at the new parent",
    );
    t.err(
        ops::rename("/tmp/t/d2", "/tmp/t/d2/inner/x"),
        Errno::EINVAL,
        "rename dir into itself",
    );
    t.ok(
        ops::stat("/tmp/t").map(|s| s.nlink).ok() == Some(3),
        "dir nlink = 2 + subdirs",
    );
    t.err(ops::rmdir("/tmp/t/d2"), Errno::ENOTEMPTY, "rmdir non-empty");
    t.err(ops::rmdir("/tmp/t/b.txt"), Errno::ENOTDIR, "rmdir a file");
    t.err(ops::unlink("/tmp/t/d2"), Errno::EISDIR, "unlink a dir");

    // getdents64 including . and .., with a tiny buffer to force several calls.
    if let Some(d) = t.must(ops::open("/tmp/t", O_RDONLY | O_DIRECTORY, 0), "open dir") {
        let mut names = Vec::new();
        let mut buf = [0u8; 40];
        let mut calls = 0;
        while let Ok(n) = d.getdents64(&mut buf) {
            if n == 0 {
                break;
            }
            names.extend(parse_dirents(&buf[..n]));
            calls += 1;
        }
        let ns: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
        t.ok(
            ns == [".", "..", "b.txt", "d2"],
            "getdents64 lists . .. b.txt d2",
        );
        t.ok(calls >= 2, "getdents64 resumes across calls");
        t.ok(
            names.iter().any(|(n, ty)| n == "d2" && *ty == DT_DIR),
            "d_type DT_DIR",
        );
        t.ok(d.seek(0, SEEK_SET).ok() == Some(0), "rewinddir");
        t.err(
            d.getdents64(&mut [0u8; 8]),
            Errno::EINVAL,
            "getdents64 buffer too small",
        );
        t.err(d.read(&mut buf), Errno::EISDIR, "read() on a directory");
    }

    // symlinks
    t.ok(ops::symlink("b.txt", "/tmp/t/link").is_ok(), "symlink");
    t.ok(
        ops::readlink("/tmp/t/link").ok().as_deref() == Some("b.txt"),
        "readlink",
    );
    t.ok(
        read_str("/tmp/t/link") == "hello world",
        "read through symlink",
    );
    t.ok(
        ops::lstat("/tmp/t/link").map(|s| s.kind).ok() == Some(InodeType::Symlink),
        "lstat sees the link",
    );
    t.ok(
        ops::symlink("../t/d2", "/tmp/t/dlink").is_ok(),
        "dir symlink",
    );
    t.ok(
        read_str("/tmp/t/dlink/inner/f") == "in d1",
        "walk through dir symlink",
    );
    t.ok(
        ops::symlink("newfile", "/tmp/t/dangling").is_ok(),
        "dangling symlink",
    );
    t.ok(
        ops::open("/tmp/t/dangling", O_WRONLY | O_CREAT, 0o644).is_ok()
            && ops::stat("/tmp/t/newfile").is_ok(),
        "O_CREAT through a dangling symlink creates the target",
    );
    t.ok(
        ops::symlink("loop2", "/tmp/t/loop1").is_ok()
            && ops::symlink("loop1", "/tmp/t/loop2").is_ok(),
        "symlink loop",
    );
    t.err(
        ops::stat("/tmp/t/loop1"),
        Errno::ELOOP,
        "symlink loop gives ELOOP",
    );
    t.err(
        ops::open("/tmp/t/link", O_RDONLY | O_NOFOLLOW, 0),
        Errno::ELOOP,
        "O_NOFOLLOW on a symlink",
    );
    t.ok(
        ops::open("/tmp/t/link", O_PATH | O_NOFOLLOW, 0)
            .map(|f| f.stat().kind)
            .ok()
            == Some(InodeType::Symlink),
        "O_PATH|O_NOFOLLOW opens the link itself",
    );
    t.ok(
        ops::open("/tmp/t/link", O_PATH, 0)
            .map(|f| f.stat().kind)
            .ok()
            == Some(InodeType::File),
        "O_PATH follows",
    );

    // hard links, and unlink while open
    t.ok(ops::link("/tmp/t/b.txt", "/tmp/t/hard").is_ok(), "link");
    t.ok(
        ops::stat("/tmp/t/b.txt").map(|s| s.nlink).ok() == Some(2),
        "nlink 2",
    );
    let open_b = ops::open("/tmp/t/b.txt", O_RDONLY, 0).ok();
    t.ok(ops::unlink("/tmp/t/b.txt").is_ok(), "unlink b.txt");
    t.ok(
        read_str("/tmp/t/hard") == "hello world",
        "other link still reads",
    );
    if let Some(f) = open_b {
        let mut buf = [0u8; 5];
        t.ok(
            f.read(&mut buf).ok() == Some(5) && &buf == b"hello",
            "open fd survives unlink",
        );
    }

    // clean up everything
    for p in [
        "/tmp/t/hard",
        "/tmp/t/link",
        "/tmp/t/dlink",
        "/tmp/t/dangling",
        "/tmp/t/newfile",
        "/tmp/t/loop1",
        "/tmp/t/loop2",
        "/tmp/t/d2/inner/f",
    ] {
        t.ok(ops::unlink(p).is_ok(), "cleanup unlink");
    }
    t.ok(
        ops::rmdir("/tmp/t/d2/inner").is_ok()
            && ops::rmdir("/tmp/t/d2").is_ok()
            && ops::rmdir("/tmp/t").is_ok(),
        "cleanup rmdir",
    );
    t.err(ops::stat("/tmp/t"), Errno::ENOENT, "/tmp/t gone");
}

fn errno_paths(t: &mut T) {
    t.err(
        ops::open("/nope", O_RDONLY, 0),
        Errno::ENOENT,
        "open missing",
    );
    t.err(
        ops::open("/nope/x", O_RDONLY | O_CREAT, 0o644),
        Errno::ENOENT,
        "create in missing dir",
    );
    t.err(
        ops::open("/System/Settings/motd", O_RDONLY | O_CREAT | O_EXCL, 0),
        Errno::EEXIST,
        "O_EXCL on existing",
    );
    t.err(
        ops::open("/System/Settings/motd/x", O_RDONLY, 0),
        Errno::ENOTDIR,
        "file used as dir",
    );
    t.err(
        ops::open("/System/Settings/motd/", O_RDONLY, 0),
        Errno::ENOTDIR,
        "trailing slash on a file",
    );
    t.err(
        ops::open("/System/Settings", O_WRONLY, 0),
        Errno::EISDIR,
        "open dir for writing",
    );
    t.err(
        ops::open("/System/Settings/motd", O_RDONLY | O_DIRECTORY, 0),
        Errno::ENOTDIR,
        "O_DIRECTORY on a file",
    );
    t.err(ops::open("", O_RDONLY, 0), Errno::ENOENT, "empty path");
    t.err(
        ops::open("/System/Settings/motd", O_ACCMODE, 0),
        Errno::EINVAL,
        "bad access mode",
    );
    t.err(ops::mkdir("/tmp", 0o755), Errno::EEXIST, "mkdir existing");
    t.err(ops::mkdir("/", 0o755), Errno::EEXIST, "mkdir /");
    t.err(ops::rmdir("/"), Errno::EBUSY, "rmdir /");
    t.err(ops::rmdir("/tmp/."), Errno::EINVAL, "rmdir .");
    let long: String = core::iter::repeat_n('a', 300).collect();
    let mut p = String::from("/tmp/");
    p.push_str(&long);
    t.err(
        ops::open(&p, O_RDONLY | O_CREAT, 0o644),
        Errno::ENAMETOOLONG,
        "name > 255",
    );
    if let Some(f) = t.must(ops::open("/System/Settings/motd", O_RDONLY, 0), "open motd") {
        t.err(f.write(b"x"), Errno::EBADF, "write on O_RDONLY");
        t.err(f.truncate(0), Errno::EINVAL, "ftruncate on O_RDONLY");
    }
    if let Some(f) = t.must(
        ops::open("/tmp/wo", O_WRONLY | O_CREAT, 0o644),
        "open O_WRONLY",
    ) {
        t.err(f.read(&mut [0u8; 4]), Errno::EBADF, "read on O_WRONLY");
    }
    t.ok(ops::unlink("/tmp/wo").is_ok(), "unlink /tmp/wo");
    t.err(
        ops::rename("/System/Settings/hostname", "/dev/hostname"),
        Errno::EXDEV,
        "rename across mounts",
    );
    t.err(ops::rmdir("/dev"), Errno::EBUSY, "rmdir a mount point");
    t.err(
        ops::rename("/dev", "/devices"),
        Errno::EBUSY,
        "rename a mount point",
    );
    t.err(
        ops::open("/dev/newfile", O_WRONLY | O_CREAT, 0o644),
        Errno::EROFS,
        "create in devfs",
    );
}

fn paths_and_mounts(t: &mut T) {
    t.ok(
        read_str("/Users/milk/../../System/Settings/hostname") == "milk\n",
        "../.. walk",
    );
    t.ok(
        read_str("//System/Settings///hostname") == "milk\n",
        "repeated slashes",
    );
    t.ok(
        [
            "/bin",
            "/etc",
            "/home",
            "/srv",
            "/run",
            "/sys",
            "/Applications",
            "/System/Library",
        ]
        .iter()
        .all(|p| ops::lstat(p).is_err()),
        "no old Unix folders (clean break; /run and /sys are under /System)",
    );
    t.ok(
        ops::lookup("/dev/..").map(|i| i.ino()).ok() == Some(path::root().ino()),
        "/dev/.. is /",
    );
    t.ok(
        ops::lookup("/..").map(|i| i.ino()).ok() == Some(path::root().ino()),
        "/.. is /",
    );
    let pathof = |p: &str| {
        ops::lookup(p)
            .and_then(|i| path::path_of(&i))
            .unwrap_or_default()
    };
    t.ok(
        pathof("/Users/milk") == "/Users/milk",
        "path_of /Users/milk",
    );
    t.ok(
        pathof("/System/Settings/milk/..") == "/System/Settings",
        "path_of through ..",
    );
    t.ok(
        pathof("/dev") == "/dev" && pathof("/") == "/",
        "path_of across a mount",
    );
    if let Some(home) = t.must(ops::lookup("/Users"), "lookup /Users") {
        t.ok(
            ops::stat_at(Some(&home), "milk/README.txt", true).is_ok(),
            "relative path from a base dir",
        );
        t.ok(
            ops::stat_at(Some(&home), "/System/Settings/motd", true).is_ok(),
            "absolute path ignores the base",
        );
    }
}

fn initramfs(t: &mut T) {
    t.ok(
        read_str("/System/Settings/hostname") == "milk\n",
        "/System/Settings/hostname",
    );
    t.ok(
        read_str("/System/Settings/motd").contains("Welcome to milkOS"),
        "/System/Settings/motd",
    );
    t.ok(
        read_str("/System/Version.txt").contains("ProductName:    milkOS"),
        "/System/Version.txt",
    );
    t.ok(
        read_str("/Users/milk/README.txt").starts_with("Hi"),
        "/Users/milk/README.txt",
    );
    t.ok(
        ops::stat("/tmp").map(|s| s.perm).ok() == Some(0o1777),
        "/tmp is sticky + world-writable",
    );
    for d in [
        "/Apps",
        "/Commands",
        "/System",
        "/System/Services",
        "/System/Settings",
        "/System/Startup",
        "/System/Kernel",
        "/System/Services/Sockets",
        "/System/Developer",
        "/Users/milk",
        "/Volumes",
        "/dev",
        "/proc",
        "/tmp",
    ] {
        t.ok(
            ops::stat(d).map(|s| s.kind).ok() == Some(InodeType::Dir),
            "skeleton dir exists",
        );
    }
    t.ok(
        ["hello", "fault", "utest", "fsstress", "linux-hello"]
            .iter()
            .all(|n| ops::lstat(&alloc::format!("/Commands/{n}")).is_err()),
        "test tools are in /System/Developer, not /Commands",
    );
    let shown: Vec<String> = ops::readdir("/")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .filter(|n| !filekit::path::hidden("/", n))
        .collect();
    t.ok(
        ["Apps", "Commands", "System", "Users", "Volumes"]
            .iter()
            .all(|n| shown.iter().any(|x| x == n))
            && !shown
                .iter()
                .any(|n| n == "dev" || n == "proc" || n == "tmp"),
        "a listing of / hides dev, proc and tmp",
    );
}

fn devfs(t: &mut T) {
    let st = ops::stat("/dev/null");
    t.ok(
        st.as_ref().map(|s| (s.kind, s.rdev)).ok() == Some((InodeType::CharDev, makedev(1, 3))),
        "/dev/null is char 1:3",
    );
    if let Some(n) = t.must(ops::open("/dev/null", O_RDWR, 0), "open /dev/null") {
        t.ok(n.read(&mut [0u8; 8]).ok() == Some(0), "/dev/null reads EOF");
        t.ok(
            n.write(b"gone").ok() == Some(4),
            "/dev/null swallows writes",
        );
    }
    t.ok(
        ops::open("/dev/null", O_WRONLY | O_TRUNC, 0).is_ok(),
        "> /dev/null (O_TRUNC) works",
    );
    if let Some(z) = t.must(ops::open("/dev/zero", O_RDONLY, 0), "open /dev/zero") {
        let mut b = [0xffu8; 100];
        t.ok(
            z.read(&mut b).ok() == Some(100) && b.iter().all(|&x| x == 0),
            "/dev/zero",
        );
    }
    if let Some(r) = t.must(ops::open("/dev/random", O_RDONLY, 0), "open /dev/random") {
        let (mut a, mut b) = ([0u8; 16], [0u8; 16]);
        let _ = r.read(&mut a);
        let _ = r.read(&mut b);
        t.ok(a != b && a != [0; 16], "/dev/random gives different bytes");
    }
    if let Some(c) = t.must(ops::open("/dev/console", O_RDWR, 0), "open /dev/console") {
        t.ok(
            c.write(b"fs selftest: hello from /dev/console\n").ok() == Some(37),
            "write /dev/console",
        );
        if crate::uart::available() == 0 {
            t.err(
                c.read(&mut [0u8; 4]),
                Errno::EAGAIN,
                "/dev/console read with no input",
            );
        }
    }
    let names: Vec<String> = ops::readdir("/dev")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    for n in ["null", "zero", "random", "urandom", "console"] {
        t.ok(names.iter().any(|x| x == n), "/dev entry listed");
    }
}

fn procfs_sysfs(t: &mut T) {
    let mi = read_str("/proc/meminfo");
    t.ok(
        mi.starts_with("MemTotal:") && mi.contains("MemFree:") && mi.contains("KernelHeap:"),
        "/proc/meminfo",
    );
    let total_kb: usize = mi
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    t.ok(
        total_kb == pmm::stats().total_frames * 4,
        "MemTotal matches the PMM",
    );
    // The breakdown after Shmem adds up to MemTotal - MemFree (each line rounds down on
    // its own, so it may fall short by under 1 kB a line), and the heap's frames are
    // exactly its size.
    let kb = |key: &str| -> usize {
        mi.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
            .unwrap_or(usize::MAX / 4)
    };
    let parts = [
        "KernelHeap",
        "KernelImage",
        "BuiltinFiles",
        "BootReserved",
        "PageTables",
        "KernelStacks",
        "AppPages",
        "RootFiles",
        "PageCache",
        "Display",
        "Wallpaper",
        "WindowBuffers",
        "PanelCaches",
        "Devices",
        "Other",
    ];
    let sum: usize = parts.iter().map(|k| kb(k)).sum();
    let used = kb("MemTotal") - kb("MemFree");
    t.ok(
        sum <= used && used - sum < parts.len(),
        "/proc/meminfo breakdown adds up to MemTotal - MemFree",
    );
    t.ok(
        crate::heap::stats().total == pmm::stats().of(pmm::Use::Heap) * 4096,
        "the kernel heap's frames are tagged Heap",
    );
    let up = read_str("/proc/uptime");
    let secs: f32 = up
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1.0);
    t.ok(secs > 0.5 && secs < 600.0, "/proc/uptime is plausible");
    t.ok(
        read_str("/proc/version").starts_with(concat!("milkOS ", env!("MILKOS_VERSION"))),
        "/proc/version",
    );
    let irqs = read_str("/proc/interrupts");
    let timer_count: u64 = irqs
        .lines()
        .find(|l| l.trim_start().starts_with("27:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    t.ok(timer_count > 50, "/proc/interrupts counts timer ticks");
    t.ok(
        irqs.contains("uart-pl011"),
        "/proc/interrupts lists the UART",
    );
    let mounts = read_str("/proc/mounts");
    t.ok(
        mounts.contains("tmpfs / tmpfs")
            && mounts.contains("devfs /dev devfs")
            && mounts.contains("proc /proc proc")
            && mounts.contains("sysfs /System/Kernel sysfs"),
        "/proc/mounts",
    );
    t.err(
        ops::open("/proc/version", O_WRONLY, 0).and_then(|f| f.write(b"x")),
        Errno::EACCES,
        "procfs is read-only",
    );
    t.err(
        ops::mkdir("/proc/x", 0o755),
        Errno::EROFS,
        "mkdir in procfs",
    );
    t.ok(
        ops::stat("/proc/meminfo").map(|s| s.size).ok() == Some(0),
        "procfs st_size 0",
    );
    let devs = ops::readdir("/System/Kernel/devices/platform").unwrap_or_default();
    t.ok(
        devs.iter().any(|e| e.name == "9000000.pl011"),
        "/System/Kernel/devices/platform lists the UART",
    );
    t.ok(
        read_str("/System/Kernel/devices/platform/9000000.pl011/compatible").contains("arm,pl011"),
        "sysfs compatible",
    );
    t.ok(
        read_str("/System/Kernel/devices/platform/9000000.pl011/reg")
            .starts_with("0x9000000 0x1000"),
        "sysfs reg",
    );
    t.ok(
        read_str("/System/Kernel/kernel/version") == read_str("/proc/version"),
        "/System/Kernel/kernel/version",
    );
    t.ok(
        ops::lookup("/System/Kernel/devices/platform/..")
            .and_then(|i| path::path_of(&i))
            .ok()
            .as_deref()
            == Some("/System/Kernel/devices"),
        "sysfs .. and path_of",
    );
}

/// Disk I/O through the first block device, if there is one. Non-destructive: the region
/// written (the last 80 KiB, bigger than the driver's 64 KiB bounce buffer) is restored.
fn blockdev(t: &mut T) {
    // The scratch disk: the first one that does not hold milkfs.
    let Some(d) = super::blockdev::devices()
        .into_iter()
        .find(|d| super::milkfs::probe(d).is_none() && super::fat::probe(d).is_none())
    else {
        kprintln!("fs selftest: no block device, skipping disk checks");
        return;
    };
    kprintln!("fs selftest: disk {}", super::blockdev::describe(&d));
    let bs = d.block_size();
    let mut s0 = vec![0u8; bs];
    if t.must(d.read_blocks(0, &mut s0), "read sector 0").is_some() {
        let line: String = s0
            .iter()
            .take_while(|&&b| b != b'\n' && b != 0)
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        kprintln!("fs selftest: sector 0 says: {line:?}");
    }
    let sectors = 160u64;
    if d.num_blocks() < sectors + 1 || d.read_only() {
        return;
    }
    let lba = d.num_blocks() - sectors;
    let len = sectors as usize * bs;
    let mut saved = vec![0u8; len];
    if t.must(d.read_blocks(lba, &mut saved), "read test region")
        .is_none()
    {
        return;
    }
    let pattern: Vec<u8> = (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761) as u8 ^ (i >> 9) as u8)
        .collect();
    t.ok(d.write_blocks(lba, &pattern).is_ok(), "write 80 KiB");
    t.ok(d.flush().is_ok(), "flush");
    let mut back = vec![0u8; len];
    t.ok(
        d.read_blocks(lba, &mut back).is_ok() && back == pattern,
        "80 KiB reads back from the disk",
    );
    t.err(
        d.read_blocks(d.num_blocks(), &mut back[..bs]),
        Errno::ENXIO,
        "read past the end",
    );
    t.err(
        d.read_blocks(0, &mut back[..100]),
        Errno::EINVAL,
        "partial-sector read",
    );

    // The same disk through /dev, at byte offsets.
    let path = alloc::format!("/dev/{}", d.name());
    let st = ops::stat(&path);
    t.ok(
        st.as_ref().map(|s| (s.kind, s.size)).ok()
            == Some((InodeType::BlockDev, d.num_blocks() * bs as u64)),
        "/dev/vdX is a block device of the right size",
    );
    if let Some(f) = t.must(ops::open(&path, O_RDWR, 0), "open /dev/vdX") {
        let off = lba * bs as u64 + 500; // straddles a sector boundary
        let mut b = [0u8; 30];
        t.ok(
            f.pread(&mut b, off).ok() == Some(30) && b[..] == pattern[500..530],
            "unaligned pread through /dev",
        );
        t.ok(
            f.pwrite(b"milkOS was here", off).ok() == Some(15),
            "unaligned pwrite",
        );
        let mut sec = vec![0u8; 2 * bs];
        let _ = d.read_blocks(lba, &mut sec);
        t.ok(
            &sec[500..515] == b"milkOS was here"
                && sec[515..] == pattern[515..2 * bs]
                && sec[..500] == pattern[..500],
            "read-modify-write kept the neighbours",
        );
    }
    // Block cache over the same region: 4 buffers, 6 blocks written, so evictions write
    // back early; everything must be on the disk after flush.
    if lba % 8 == 0
        && let Some(c) = t.must(super::bcache::BCache::new(d.clone(), 4), "bcache")
    {
        let base = lba / 8;
        let blk = |n: u64| -> super::bcache::Block { [(n as u8).wrapping_mul(37) ^ 0x5a; 4096] };
        for n in 0..6 {
            let _ = c.write(base + n, &blk(n));
        }
        let mut b = [0u8; 4096];
        t.ok(
            (0..6).all(|n| c.read(base + n, &mut b).is_ok() && b == blk(n)),
            "bcache reads its own writes",
        );
        let st = c.stats();
        t.ok(
            st.writebacks >= 2 && st.misses >= 6,
            "bcache evicted and wrote back",
        );
        t.ok(
            c.flush().is_ok() && c.usage().2 == 0,
            "bcache flush leaves nothing dirty",
        );
        let mut raw = vec![0u8; 4096];
        t.ok(
            (0..6)
                .all(|n| d.read_blocks((base + n) * 8, &mut raw).is_ok() && raw[..] == blk(n)[..]),
            "bcache data is on the disk after flush",
        );
        t.err(
            c.read(c.num_blocks(), &mut b),
            Errno::ENXIO,
            "bcache read past the end",
        );
    }
    t.ok(
        d.write_blocks(lba, &saved).is_ok()
            && d.read_blocks(lba, &mut back).is_ok()
            && back == saved,
        "test region restored",
    );
}

/// `rm -r`, best effort.
pub fn rm_rf(path: &str) {
    match ops::lstat(path) {
        Ok(st) if st.kind == InodeType::Dir => {
            for e in ops::readdir(path).unwrap_or_default() {
                rm_rf(&alloc::format!("{path}/{}", e.name));
            }
            let _ = ops::rmdir(path);
        }
        Ok(_) => {
            let _ = ops::unlink(path);
        }
        Err(_) => {}
    }
}

/// The Mac shared folder, if qemu provides one: read-only checks only (never writes into
/// the user's folder, even when it is mounted read-write).
fn mac_share(t: &mut T) {
    let Ok(st) = ops::stat("/Volumes/Mac") else {
        kprintln!("fs selftest: no Mac shared folder, skipping 9p checks");
        return;
    };
    t.ok(st.kind == InodeType::Dir, "9p: /Volumes/Mac is a directory");
    t.ok(
        read_str("/proc/mounts").contains("9p /Volumes/Mac 9p"),
        "9p in /proc/mounts",
    );
    let names: Vec<String> = ops::readdir("/Volumes/Mac")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    t.ok(
        names.iter().any(|n| n == "README.txt"),
        "9p readdir sees README.txt",
    );
    t.ok(
        read_str("/Volumes/Mac/README.txt").starts_with("milkOS shared folder"),
        "9p read",
    );
    t.ok(
        ops::stat("/Volumes/Mac/README.txt")
            .is_ok_and(|s| s.kind == InodeType::File && s.size > 100),
        "9p stat",
    );
    t.ok(
        ops::lookup("/Volumes/Mac/..").map(|i| i.ino()).ok()
            == ops::lookup("/Volumes").map(|i| i.ino()).ok(),
        "9p .. leaves the share",
    );
    t.err(
        ops::stat("/Volumes/Mac/no-such-file"),
        Errno::ENOENT,
        "9p ENOENT",
    );
    if !super::ninep::is_rw() {
        t.err(
            ops::open("/Volumes/Mac/.selftest", O_WRONLY | O_CREAT, 0o644),
            Errno::EROFS,
            "9p read-only by default",
        );
    }
}

/// Space accounting on the mounted milkfs must be exact after every phase.
fn milk_space(t: &mut T, v: &super::milkfs::Volume, what: &str) {
    let r = v.state.lock().verify_space();
    match r {
        Ok(c) => t.ok(
            c.free_but_used == 0 && c.leaked == 0 && c.reachable == c.allocated,
            &alloc::format!("{what} ({c:?})"),
        ),
        Err(e) => t.ok(false, &alloc::format!("{what}: {e}")),
    }
}

fn milkfs(t: &mut T) {
    use super::milkfs::format::xxh64;
    t.ok(xxh64(b"") == 0xef46_db37_51d8_e999, "xxh64 empty");
    t.ok(xxh64(b"abc") == 0x44bc_2cf5_ad77_0999, "xxh64 abc");
    t.ok(
        xxh64(b"Nobody inspects the spammish repetition") == 0xfbce_a83c_8a37_8bf1,
        "xxh64 long",
    );
    let hundred: Vec<u8> = (0..100).collect();
    t.ok(
        xxh64(&hundred) == 0x6ac1_e580_3216_6597,
        "xxh64 matches mkmilkfs.py",
    );

    milkfs_user_volume(t);
    milkfs_test_volume(t);
}

/// The user's volume: read-only checks only. Never writes, never snapshots.
fn milkfs_user_volume(t: &mut T) {
    let (Some(v), Some(vp)) = (super::milkfs::user_volume(), super::milkfs::mount_path()) else {
        kprintln!("fs selftest: no milkfs volume, skipping the user-volume checks");
        return;
    };
    t.ok(
        ops::stat(&vp).map(|s| s.kind).ok() == Some(InodeType::Dir),
        "user volume mounted",
    );
    t.ok(
        read_str("/proc/mounts").contains(&alloc::format!("milkfs {vp} milkfs")),
        "user volume in /proc/mounts",
    );
    let generation = v.state.lock().sb.generation;
    let info = read_str("/System/Kernel/fs/milkfs/info");
    t.ok(
        info.contains("state:       ok")
            && info.contains(&alloc::format!("generation:  {generation}")),
        "user volume info consistent",
    );
    milk_space(t, &v, "user volume space accounting exact");
    // Persistent home: /Users/milk is a bind mount of <volume>/Users/milk.
    let vol_dev = ops::stat(&vp).map(|s| s.dev).ok();
    t.ok(
        ops::stat("/Users/milk").map(|s| s.dev).ok() == vol_dev,
        "/Users/milk is on milkfs",
    );
    let pathof = |p: &str| {
        ops::lookup(p)
            .and_then(|i| path::path_of(&i))
            .unwrap_or_default()
    };
    t.ok(
        pathof("/Users/milk") == "/Users/milk",
        "path_of home is /Users/milk",
    );
    t.ok(
        ops::stat("/Apps").map(|s| s.dev).ok() == vol_dev && pathof("/Apps") == "/Apps",
        "/Apps is on milkfs",
    );
    t.err(
        ops::rmdir(&alloc::format!("{vp}/Users/milk")),
        Errno::EBUSY,
        "the bound home can't be removed",
    );
}

/// Everything destructive, on a throwaway milkfs in RAM (mkfs'd here, unmounted after).
fn milkfs_test_volume(t: &mut T) {
    use super::blockdev::BlockRef;
    const VP: &str = "/Volumes/.milk-selftest";
    const D: &str = "/Volumes/.milk-selftest/t";
    // Pages cached before the test (the boot copying the built-in apps onto the user
    // volume) would come back at the drop_all below and spoil the count: drop them first.
    super::pagecache::drop_all();
    let free0 = pmm::stats().free_frames;
    let heap0 = crate::heap::stats().total;
    let Some(disk) = t.must(super::ramdisk::RamDisk::new("ram0", 16 << 20), "RAM disk") else {
        return;
    };
    let dev: BlockRef = alloc::sync::Arc::new(disk);
    t.ok(
        super::milkfs::mkfs(&dev, "MilkTest", false).is_ok(),
        "mkfs the test volume",
    );
    t.err(
        super::milkfs::mkfs(&dev, "MilkTest", false),
        Errno::EEXIST,
        "mkfs refuses an existing milkfs",
    );
    let Some((vol, report)) = t.must(
        super::milkfs::mount_secondary(dev.clone(), VP),
        "mount the test volume",
    ) else {
        return;
    };
    t.ok(
        report.problems == 0 && report.inodes == 1,
        "fresh volume mounts clean",
    );
    super::milkfs::set_test_override(Some((vol.clone(), VP)));
    milkfs_destructive(t, &vol, VP, D, &dev);
    milkfs_clones(t, &vol, VP, &dev);
    page_cache(t, VP);
    milkfs_diff(t, VP);
    milkfs_versions(t, VP);
    milkfs_fsck(t, &vol, VP, &dev);
    milkfs_bench(&vol, VP);
    milkfs_tree(t, &vol, VP);
    milkfs_reflink(t, &vol, VP);
    milkfs_sendrecv(t, VP);
    milkfs_scrub(t, &vol, VP, &dev);
    milkfs_spacemap(t);
    milkfs_statfs(t, VP);
    milkfs_control_errors(t);
    milkfs_scrub_keyring(t);
    super::milkfs::set_test_override(None);
    t.ok(super::unmount(VP).is_ok(), "unmount the test volume");
    let _ = ops::rmdir(VP);
    drop(vol);
    drop(dev);
    super::pagecache::drop_all(); // cached pages hold frames too
    // Every frame comes back, except what the kernel heap grew by (it never shrinks).
    let lost = free0.saturating_sub(pmm::stats().free_frames);
    let grown = (crate::heap::stats().total - heap0) / crate::mmu::PAGE_SIZE;
    t.ok(
        lost == grown,
        &alloc::format!("test volume's RAM disk freed ({lost} frames gone, heap grew {grown})"),
    );
}

fn milkfs_destructive(
    t: &mut T,
    vol: &alloc::sync::Arc<super::milkfs::Volume>,
    vp: &str,
    d: &str,
    dev: &super::blockdev::BlockRef,
) {
    milk_space(t, vol, "milkfs space exact at start");
    t.ok(ops::mkdir(d, 0o755).is_ok(), "milkfs mkdir");
    // Bind mounts (what the persistent home uses), on the test volume.
    {
        let src = alloc::format!("{vp}/bindsrc");
        let _ = ops::mkdir(&src, 0o755);
        let _ = ops::write_file(&alloc::format!("{src}/f"), b"bound");
        let _ = ops::mkdir("/tmp/bindtest", 0o755);
        t.ok(super::bind(&src, "/tmp/bindtest").is_ok(), "bind mount");
        t.ok(
            read_str("/tmp/bindtest/f") == "bound",
            "read through a bind mount",
        );
        t.ok(
            ops::lookup("/tmp/bindtest")
                .and_then(|i| path::path_of(&i))
                .ok()
                .as_deref()
                == Some("/tmp/bindtest"),
            "path_of a bind mount",
        );
        t.err(
            ops::rmdir(&src),
            Errno::EBUSY,
            "a bound directory can't be removed",
        );
        t.ok(super::unmount("/tmp/bindtest").is_ok(), "unmount the bind");
        let _ = ops::rmdir("/tmp/bindtest");
        let _ = ops::unlink(&alloc::format!("{src}/f"));
        t.ok(
            ops::rmdir(&src).is_ok(),
            "bind source removable after unmount",
        );
    }
    let p = |n: &str| alloc::format!("{d}/{n}");

    // files
    if let Some(f) = t.must(
        ops::open(&p("a.txt"), O_RDWR | O_CREAT | O_EXCL, 0o644),
        "milkfs create",
    ) {
        t.ok(f.write(b"hello world").ok() == Some(11), "milkfs write");
        let mut b = [0u8; 16];
        t.ok(
            f.pread(&mut b, 0).ok() == Some(11) && &b[..11] == b"hello world",
            "milkfs read back",
        );
        t.ok(f.pwrite(b"W", 6).ok() == Some(1), "milkfs pwrite");
        t.ok(f.stat().size == 11 && f.stat().perm == 0o644, "milkfs stat");
    }
    t.ok(
        read_str(&p("a.txt")) == "hello World",
        "milkfs pwrite in place",
    );
    if let Some(a) = t.must(
        ops::open(&p("a.txt"), O_WRONLY | O_APPEND, 0),
        "milkfs O_APPEND",
    ) {
        let _ = a.write(b"!");
    }
    t.ok(read_str(&p("a.txt")) == "hello World!", "milkfs append");

    // big file across many blocks and tree splits
    let len = 300 * 1024 + 77;
    let data: Vec<u8> = (0..len).map(|i| (i * 13 + i / 4096) as u8).collect();
    t.ok(
        ops::write_file(&p("big"), &data).is_ok(),
        "milkfs write 300 KiB",
    );
    t.ok(
        ops::read_file(&p("big")).ok().as_deref() == Some(&data[..]),
        "milkfs 300 KiB reads back",
    );
    t.ok(
        ops::truncate(&p("big"), 5000).is_ok() && ops::truncate(&p("big"), 9000).is_ok(),
        "milkfs truncate",
    );
    let back = ops::read_file(&p("big")).unwrap_or_default();
    t.ok(
        back.len() == 9000 && back[..5000] == data[..5000] && back[5000..].iter().all(|&b| b == 0),
        "milkfs truncated tail reads as zeros",
    );
    if let Some(s) = t.must(
        ops::open(&p("sparse"), O_RDWR | O_CREAT, 0o600),
        "milkfs sparse",
    ) {
        let _ = s.pwrite(b"x", 1 << 20);
        let st = s.stat();
        t.ok(
            st.size == (1 << 20) + 1 && st.blocks == 8,
            "milkfs sparse file has one block",
        );
        let mut hole = [1u8; 32];
        t.ok(
            s.pread(&mut hole, 8192).ok() == Some(32) && hole == [0; 32],
            "milkfs hole reads zeros",
        );
    }
    for i in 0..60 {
        let _ = ops::write_file(
            &p(&alloc::format!("many-{i:02}")),
            alloc::format!("file {i}").as_bytes(),
        );
    }
    t.ok(
        (0..60).all(|i| read_str(&p(&alloc::format!("many-{i:02}"))) == alloc::format!("file {i}")),
        "milkfs 60 small files",
    );
    milk_space(t, vol, "milkfs space exact after writes");

    // namespace
    t.ok(
        ops::rename(&p("a.txt"), &p("b.txt")).is_ok(),
        "milkfs rename",
    );
    t.err(
        ops::stat(&p("a.txt")),
        Errno::ENOENT,
        "milkfs old name gone",
    );
    t.ok(
        ops::mkdir(&p("d1"), 0o755).is_ok()
            && ops::mkdir(&p("d2"), 0o755).is_ok()
            && ops::write_file(&p("d1/f"), b"in d1").is_ok(),
        "milkfs dirs",
    );
    t.ok(
        ops::rename(&p("d1"), &p("d2/inner")).is_ok(),
        "milkfs rename dir",
    );
    t.ok(
        read_str(&p("d2/inner/../../b.txt")) == "hello World!",
        "milkfs .. after rename",
    );
    t.err(
        ops::rename(&p("d2"), &p("d2/inner/x")),
        Errno::EINVAL,
        "milkfs dir into itself",
    );
    t.err(
        ops::rmdir(&p("d2")),
        Errno::ENOTEMPTY,
        "milkfs rmdir non-empty",
    );
    t.err(ops::unlink(&p("d2")), Errno::EISDIR, "milkfs unlink dir");
    t.ok(
        ops::stat(d).map(|s| s.nlink).ok() == Some(3),
        "milkfs dir nlink",
    );
    t.ok(
        ops::symlink("b.txt", &p("link")).is_ok() && read_str(&p("link")) == "hello World!",
        "milkfs symlink",
    );
    t.ok(
        ops::link(&p("b.txt"), &p("hard")).is_ok(),
        "milkfs hard link",
    );
    t.ok(
        ops::stat(&p("b.txt")).map(|s| s.nlink).ok() == Some(2),
        "milkfs nlink 2",
    );
    let open_big = ops::open(&p("big"), O_RDONLY, 0).ok();
    t.ok(ops::unlink(&p("big")).is_ok(), "milkfs unlink open file");
    if let Some(f) = open_big {
        let mut b = [0u8; 4];
        t.ok(
            f.pread(&mut b, 0).ok() == Some(4) && b[..] == data[..4],
            "milkfs open file survives unlink",
        );
    }
    if let Some(d) = t.must(ops::open(d, O_RDONLY | O_DIRECTORY, 0), "milkfs open dir") {
        let mut buf = vec![0u8; 8192];
        let n = d.getdents64(&mut buf).unwrap_or(0);
        let names: Vec<String> = parse_dirents(&buf[..n])
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        t.ok(
            names.len() == 2 + 60 + 5
                && names.iter().any(|n| n == "d2")
                && names.iter().any(|n| n == "sparse"),
            "milkfs getdents64",
        );
    }
    t.ok(
        ops::readdir(vp)
            .unwrap_or_default()
            .iter()
            .any(|e| e.name == ".snapshots"),
        ".snapshots listed in the volume root",
    );
    t.err(
        ops::mkdir(&alloc::format!("{vp}/.snapshots"), 0o755),
        Errno::EEXIST,
        "can't create .snapshots",
    );
    t.err(
        ops::rename(&p("b.txt"), "/tmp/b.txt"),
        Errno::EXDEV,
        "milkfs rename to tmpfs",
    );
    milk_space(t, vol, "milkfs space exact after namespace ops");

    // snapshots
    let before = read_str(&p("b.txt"));
    t.ok(
        super::milkfs::snapshot("selftest-a").is_ok(),
        "snapshot selftest-a",
    );
    t.err(
        super::milkfs::snapshot("selftest-a"),
        Errno::EEXIST,
        "duplicate snapshot name",
    );
    t.err(
        super::milkfs::snapshot("bad/name"),
        Errno::EINVAL,
        "bad snapshot name",
    );
    let _ = ops::write_file(&p("b.txt"), b"changed after the snapshot");
    let _ = ops::write_file(&p("new.txt"), b"born after the snapshot");
    let _ = ops::unlink(&p("many-00"));
    let snap = &alloc::format!("{vp}/.snapshots/selftest-a/t");
    t.ok(
        read_str(&alloc::format!("{snap}/b.txt")) == before,
        "snapshot keeps the old content",
    );
    t.ok(
        read_str(&p("b.txt")) == "changed after the snapshot",
        "live has the new content",
    );
    t.err(
        ops::stat(&alloc::format!("{snap}/new.txt")),
        Errno::ENOENT,
        "snapshot doesn't see new files",
    );
    t.ok(
        read_str(&alloc::format!("{snap}/many-00")) == "file 0",
        "snapshot keeps deleted files",
    );
    t.err(
        ops::open(&alloc::format!("{snap}/b.txt"), O_WRONLY, 0).and_then(|f| f.write(b"x")),
        Errno::EROFS,
        "snapshots are read-only",
    );
    t.err(
        ops::mkdir(&alloc::format!("{snap}/x"), 0o755),
        Errno::EROFS,
        "no mkdir in a snapshot",
    );
    t.ok(
        ops::lookup(&alloc::format!("{snap}/../.."))
            .and_then(|i| path::path_of(&i))
            .ok()
            .as_deref()
            == Some(alloc::format!("{vp}/.snapshots").as_str()),
        "snapshot .. leads to .snapshots",
    );
    milk_space(t, vol, "milkfs space exact with a snapshot");

    // the shell interface
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/snapshot", b"selftest-b\n").is_ok(),
        "snapshot via /System/Kernel",
    );
    let list = read_str("/System/Kernel/fs/milkfs/snapshots");
    t.ok(
        list.contains("selftest-a gen=") && list.contains("selftest-b gen="),
        "/System/Kernel/fs/milkfs/snapshots",
    );
    t.ok(
        read_str("/System/Kernel/fs/milkfs/info").contains("label:       MilkTest"),
        "/System/Kernel/fs/milkfs/info",
    );
    let _ = ops::write_file(&p("b.txt"), b"third version");
    t.ok(
        read_str(&alloc::format!("{vp}/.snapshots/selftest-b/t/b.txt"))
            == "changed after the snapshot",
        "second snapshot",
    );
    t.err(
        super::milkfs::mkfs(dev, "x", true),
        Errno::EBUSY,
        "mkfs refuses the mounted volume",
    );

    // rollback to the first snapshot (deletes selftest-b)
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/rollback", b"selftest-a").is_ok(),
        "rollback via /System/Kernel",
    );
    t.ok(
        read_str(&p("b.txt")) == before,
        "rollback restored the content",
    );
    t.err(
        ops::stat(&p("new.txt")),
        Errno::ENOENT,
        "rollback removed the new file",
    );
    t.ok(
        read_str(&p("many-00")) == "file 0",
        "rollback restored the deleted file",
    );
    t.ok(
        super::milkfs::snapshots()
            .map(|l| {
                l.iter()
                    .filter(|s| s.name.starts_with("selftest-"))
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>()
            })
            .ok()
            == Some(alloc::vec![String::from("selftest-a")]),
        "rollback dropped the newer snapshot",
    );
    milk_space(t, vol, "milkfs space exact after rollback");
    t.ok(
        super::milkfs::delete_snapshot("selftest-a").is_ok(),
        "delete snapshot",
    );
    t.err(
        ops::stat(&alloc::format!("{vp}/.snapshots/selftest-a")),
        Errno::ENOENT,
        "deleted snapshot is gone",
    );
    milk_space(t, vol, "milkfs space exact after deleting the snapshot");

    // Time Machine pieces (the thread itself only runs once the scheduler is up).
    {
        use super::milkfs::timemachine::{Config, prune_plan, stamp};
        t.ok(
            stamp(0) == "1970-01-01T00:00:00"
                && stamp(1_792_554_055) == "2026-10-21T03:40:55"
                && stamp(951_782_400) == "2000-02-29T00:00:00"
                && stamp(4_102_444_799) == "2099-12-31T23:59:59",
            "RTC timestamps to dates",
        );
        let day = 86_400 * 20_000;
        let mk = |g: u64, t: u64| super::milkfs::SnapInfo {
            name: alloc::format!("auto-{g}"),
            generation: g,
            created: t,
        };
        // 12 hourly today (gens 10..21), one each on the 3 previous days (gens 1..3).
        let mut list: Vec<_> = (0..12).map(|h| mk(10 + h, day + h * 3600)).collect();
        list.extend((1..=3).map(|d| mk(d, day - (4 - d) * 86_400)));
        let mut del = prune_plan(&list, 4, 3);
        del.sort();
        let mut want: Vec<String> = (10..18).map(|g| alloc::format!("auto-{g}")).collect();
        want.push(String::from("auto-1"));
        want.sort();
        t.ok(
            del == want,
            "retention: newest 4 + newest of each of 3 days",
        );
        let v = vol.clone();
        let saved = v.state.lock().auto_config();
        t.ok(
            super::ops::write_file(
                "/System/Kernel/fs/milkfs/auto",
                b"interval=30s keep=3 daily=2",
            )
            .is_ok()
                && v.state.lock().sb.auto == [1, 30, 3, 2]
                && read_str("/System/Kernel/fs/milkfs/auto")
                    .starts_with("on interval=30s keep=3 daily=2"),
            "/System/Kernel/fs/milkfs/auto sets and persists the settings",
        );
        t.err(
            super::ops::write_file("/System/Kernel/fs/milkfs/auto", b"interval=x"),
            Errno::EINVAL,
            "bad auto setting",
        );
        let _ = ops::write_file(&p("tm"), b"changed");
        let first = v.state.lock().auto_snapshot(1_792_554_055);
        let again = v.state.lock().auto_snapshot(1_792_554_056);
        t.ok(
            matches!(&first, Ok(Some(s)) if s.name == "auto-2026-10-21T03:40:55")
                && matches!(again, Ok(None)),
            "auto snapshot named by time, skipped when nothing changed",
        );
        let _ = super::milkfs::delete_snapshot("auto-2026-10-21T03:40:55");
        let _ = v.state.lock().set_auto_config(Config { ..saved });
        milk_space(t, vol, "milkfs space exact after auto snapshots");
    }

    // clean up
    let entries = ops::readdir(d).unwrap_or_default();
    for e in &entries {
        if e.kind != InodeType::Dir {
            let _ = ops::unlink(&p(&e.name));
        }
    }
    let _ = ops::unlink(&p("d2/inner/f"));
    let _ = ops::rmdir(&p("d2/inner"));
    let _ = ops::rmdir(&p("d2"));
    t.ok(ops::rmdir(d).is_ok(), "milkfs cleanup");
    milk_space(t, vol, "milkfs space exact after cleanup");
}

/// Clones on the test volume: try-before-install end to end.
fn milkfs_clones(
    t: &mut T,
    vol: &alloc::sync::Arc<super::milkfs::Volume>,
    vp: &str,
    dev: &super::blockdev::BlockRef,
) {
    use super::milkfs as m;
    let live = |n: &str| alloc::format!("{vp}/c/{n}");
    let _ = ops::mkdir(&alloc::format!("{vp}/c"), 0o755);
    let _ = ops::write_file(&live("config"), b"v1");
    let _ = ops::write_file(&live("keep"), b"unchanged");
    t.ok(m::snapshot("base").is_ok(), "snapshot for cloning");
    let Some(cp) = t.must(m::clone("base", "try"), "clone base -> try") else {
        return;
    };
    t.ok(
        cp == "/Volumes/MilkTest-try",
        "clone mounted at /Volumes/<label>-<name>",
    );
    let inc = |n: &str| alloc::format!("{cp}/c/{n}");
    t.ok(
        read_str(&inc("config")) == "v1",
        "clone starts as the snapshot",
    );
    t.ok(
        ops::write_file(&inc("config"), b"v2").is_ok()
            && ops::write_file(&inc("installed.app"), b"new app").is_ok(),
        "clone is writable",
    );
    t.ok(
        read_str(&inc("config")) == "v2" && read_str(&live("config")) == "v1",
        "clone writes don't touch the live volume",
    );
    t.err(
        ops::stat(&live("installed.app")),
        Errno::ENOENT,
        "live doesn't see the clone's files",
    );
    t.ok(
        read_str(&alloc::format!("{vp}/.snapshots/base/c/config")) == "v1",
        "the origin snapshot doesn't change",
    );
    t.err(
        ops::rename(&inc("config"), &live("x")),
        Errno::EXDEV,
        "rename between clone and live",
    );
    t.err(
        m::delete_snapshot("base"),
        Errno::EBUSY,
        "a clone's origin can't be deleted",
    );
    t.ok(
        read_str("/System/Kernel/fs/milkfs/clones")
            .contains("try of=base at=/Volumes/MilkTest-try"),
        "/System/Kernel/fs/milkfs/clones",
    );
    milk_space(t, vol, "milkfs space exact with a clone");

    // Discard: a second clone with a big file, thrown away.
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/clone", b"base scratch").is_ok(),
        "clone via /System/Kernel",
    );
    let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    t.ok(
        ops::write_file("/Volumes/MilkTest-scratch/big", &big).is_ok(),
        "write in the second clone",
    );
    milk_space(t, vol, "milkfs space exact with two clones");
    let free_with = vol.state.lock().store.free_blocks();
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/discard", b"scratch").is_ok(),
        "discard via /System/Kernel",
    );
    let _ = super::sync_all();
    t.ok(
        vol.state.lock().store.free_blocks() >= free_with + 49,
        "discard frees the clone's blocks",
    );
    t.err(
        ops::stat("/Volumes/MilkTest-scratch"),
        Errno::ENOENT,
        "discarded clone unmounted",
    );
    milk_space(t, vol, "milkfs space exact after discard");

    // The clone record and its tree survive a re-open of the device.
    let _ = super::sync_all();
    match super::milkfs::volume::State::open(dev.clone()) {
        Ok((st2, r)) => {
            let c = st2.clone_list();
            let ok = r.problems == 0
                && c.len() == 1
                && c[0].name == "try"
                && st2
                    .lookup(super::milkfs::items::clone_view(c[0].id), 1, "c")
                    .ok()
                    .flatten()
                    .and_then(|d| {
                        st2.lookup(
                            super::milkfs::items::clone_view(c[0].id),
                            d.ino,
                            "installed.app",
                        )
                        .ok()
                        .flatten()
                    })
                    .is_some();
            t.ok(ok, "clone persists across a re-open");
        }
        Err(e) => t.ok(false, &alloc::format!("re-open the test volume: {e}")),
    }

    // Promote: the clone becomes live, the old live is kept as before-try.
    let kept = m::promote_clone("try");
    t.ok(kept.as_deref() == Ok("before-try"), "promote clone");
    t.ok(
        read_str(&live("config")) == "v2" && read_str(&live("installed.app")) == "new app",
        "live is the clone after promote",
    );
    t.ok(
        read_str(&live("keep")) == "unchanged",
        "untouched files survive promote",
    );
    t.ok(
        read_str(&alloc::format!("{vp}/.snapshots/before-try/c/config")) == "v1",
        "old live kept as a snapshot",
    );
    t.err(
        ops::stat("/Volumes/MilkTest-try"),
        Errno::ENOENT,
        "promoted clone unmounted",
    );
    milk_space(t, vol, "milkfs space exact after promote");
    t.ok(
        m::delete_snapshot("base").is_ok(),
        "origin deletable once its clone is gone",
    );
    milk_space(t, vol, "milkfs space exact after deleting the old origin");
    t.ok(
        m::rollback("before-try").is_ok(),
        "undo the install: rollback",
    );
    t.ok(
        read_str(&live("config")) == "v1" && ops::stat(&live("installed.app")).is_err(),
        "rollback undid the promote",
    );
    milk_space(t, vol, "milkfs space exact after undo");
    t.ok(
        m::delete_snapshot("before-try").is_ok(),
        "delete before-try",
    );
    milk_space(t, vol, "milkfs space exact after clone round trip");
    rm_rf(&alloc::format!("{vp}/c"));
    milk_space(t, vol, "milkfs space exact after clone cleanup");
}

/// The page cache, on the test volume: hits, and no stale data after writes or rollbacks.
fn page_cache(t: &mut T, vp: &str) {
    use super::pagecache as pc;
    let f = alloc::format!("{vp}/pc.bin");
    let hits = || -> u64 {
        pc::stats()
            .split(", ")
            .find_map(|w| w.strip_suffix(" hits").and_then(|n| n.parse().ok()))
            .unwrap_or(0)
    };
    let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
    let _ = ops::write_file(&f, &data);
    let first = ops::read_file(&f).unwrap_or_default();
    let h0 = hits();
    let second = ops::read_file(&f).unwrap_or_default();
    t.ok(
        first == data && second == data,
        "page cache: reads are right",
    );
    t.ok(hits() >= h0 + 5, "page cache: second read hits the cache");
    let _ = ops::open(&f, O_WRONLY, 0).and_then(|w| w.pwrite(b"CHANGED", 4096));
    let after = ops::read_file(&f).unwrap_or_default();
    t.ok(
        &after[4096..4103] == b"CHANGED" && after[..4096] == data[..4096],
        "page cache: a write invalidates",
    );
    let _ = ops::truncate(&f, 100);
    t.ok(
        ops::read_file(&f).ok().as_deref() == Some(&after[..100]),
        "page cache: truncate invalidates",
    );
    let _ = ops::open(&f, O_WRONLY | O_APPEND, 0).and_then(|w| w.write(b"tail"));
    t.ok(
        read_str(&f).ends_with("tail") && read_str(&f).len() == 104,
        "page cache: append seen",
    );
    t.ok(
        super::milkfs::snapshot("pc").is_ok(),
        "page cache: snapshot",
    );
    let _ = ops::write_file(&f, b"after the snapshot");
    let _ = read_str(&f); // cache the new content
    t.ok(
        super::milkfs::rollback("pc").is_ok() && read_str(&f).len() == 104,
        "page cache: rollback invalidates",
    );
    let _ = super::milkfs::delete_snapshot("pc");
    // 4 MiB read twice on the RAM test volume (the disk numbers come from cachebench.sh).
    let big = alloc::format!("{vp}/big.bin");
    let chunk = vec![0x5au8; 64 << 10]; // in chunks: a 4 MiB Vec would grow the heap
    let wrote = ops::open(&big, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
        .and_then(|w| (0..64).try_for_each(|_| w.write(&chunk).map(|_| ())));
    if wrote.is_ok() {
        let r = pc::bench(&big);
        t.ok(r.is_ok(), "page cache bench");
        let _ = ops::unlink(&big);
    }
    let _ = ops::unlink(&f);
}

/// `snap diff` on the test volume.
fn milkfs_diff(t: &mut T, vp: &str) {
    use super::milkfs::{self as m, diff::Change};
    let p = |n: &str| alloc::format!("{vp}/dd/{n}");
    let _ = ops::mkdir_all(&p("sub"), 0o755);
    for (n, c) in [("a", "A"), ("b", "B"), ("c", "C")] {
        let _ = ops::write_file(&p(n), c.as_bytes());
    }
    for i in 0..300 {
        let _ = ops::write_file(&p(&alloc::format!("sub/f{i:03}")), b"same");
    }
    t.ok(m::snapshot("d1").is_ok(), "diff: snapshot d1");
    let _ = ops::write_file(&p("b"), b"B changed");
    let _ = ops::unlink(&p("c"));
    let _ = ops::write_file(&p("e"), b"new");
    let _ = ops::rename(&p("a"), &p("sub/a2"));
    let _ = ops::mkdir(&p("newdir"), 0o755);
    t.ok(m::snapshot("d2").is_ok(), "diff: snapshot d2");
    let _ = ops::write_file(&p("f"), b"after d2");
    let want = alloc::vec![
        Change::Modified(String::from("/dd/b")),
        Change::Removed(String::from("/dd/c")),
        Change::Added(String::from("/dd/e")),
        Change::Added(String::from("/dd/newdir")),
        Change::Renamed(String::from("/dd/a"), String::from("/dd/sub/a2")),
    ];
    let got = m::diff("d1", Some("d2"))
        .map(|r| r.changes)
        .unwrap_or_default();
    t.ok(
        got == want,
        &alloc::format!("diff d1..d2 is exact ({got:?})"),
    );
    t.ok(
        m::diff("d2", Some("d1")).map(|r| r.changes).ok() == Some(want),
        "diff order doesn't matter",
    );
    t.ok(
        m::diff("d2", None).map(|r| r.changes).ok()
            == Some(alloc::vec![Change::Added(String::from("/dd/f"))]),
        "diff snapshot..live",
    );
    t.ok(
        m::diff("d2", Some("d2")).is_ok_and(|r| r.changes.is_empty()),
        "diff of a snapshot with itself",
    );
    // One change among 300 files: only the path to it is read.
    t.ok(m::snapshot("d3").is_ok(), "diff: snapshot d3");
    let _ = ops::write_file(&p("sub/f123"), b"different");
    match m::diff("d3", None) {
        Ok(r) => t.ok(
            r.changes == alloc::vec![Change::Modified(String::from("/dd/sub/f123"))]
                && r.nodes_read <= 8,
            &alloc::format!(
                "diff reads only changed nodes ({} read, {} skipped)",
                r.nodes_read,
                r.subtrees_skipped
            ),
        ),
        Err(e) => t.ok(false, &alloc::format!("diff d3..live: {e}")),
    }
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/diff", b"d1 d2").is_ok()
            && read_str("/System/Kernel/fs/milkfs/diff").contains("R /dd/a -> /dd/sub/a2"),
        "/System/Kernel/fs/milkfs/diff",
    );
    t.err(
        m::diff("nope", None),
        Errno::ENOENT,
        "diff of a missing snapshot",
    );
    for n in ["d1", "d2", "d3"] {
        let _ = m::delete_snapshot(n);
    }
    rm_rf(&alloc::format!("{vp}/dd"));
}

/// Version history on the test volume.
fn milkfs_versions(t: &mut T, vp: &str) {
    use super::milkfs as m;
    let f = alloc::format!("{vp}/doc.txt");
    let snaps = || -> Vec<String> {
        m::snapshots()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.name)
            .collect()
    };
    let _ = ops::write_file(&f, b"version one");
    let _ = m::snapshot("v1");
    let _ = ops::write_file(&f, b"version two, longer");
    let _ = m::snapshot("v2");
    let _ = m::snapshot("v2b"); // same content: must collapse
    let _ = ops::rename(&f, &alloc::format!("{vp}/renamed doc.txt"));
    let f = alloc::format!("{vp}/renamed doc.txt");
    let _ = ops::write_file(&f, b"three");
    let v = m::versions(&f).unwrap_or_default();
    let names: Vec<Option<String>> = v.iter().map(|x| x.snapshot.clone()).collect();
    t.ok(
        names == alloc::vec![Some(String::from("v1")), Some(String::from("v2")), None],
        &alloc::format!("versions: distinct versions, renamed file followed ({names:?})"),
    );
    t.ok(
        v.iter().map(|x| x.size).collect::<Vec<_>>() == alloc::vec![11, 19, 5]
            && v[0].hash != v[1].hash,
        "versions: sizes and hashes",
    );
    t.ok(
        ops::write_file("/System/Kernel/fs/milkfs/versions", f.as_bytes()).is_ok()
            && read_str("/System/Kernel/fs/milkfs/versions").contains("v2\t"),
        "/System/Kernel/fs/milkfs/versions",
    );
    let before = snaps().len();
    t.ok(m::restore_version(&f, "v1").is_ok(), "restore a version");
    t.ok(read_str(&f) == "version one", "restored content");
    t.ok(
        snaps().len() == before + 1 && snaps().iter().any(|s| s.starts_with("pre-restore-")),
        "restore kept the unsaved current version in a snapshot",
    );
    let before = snaps().len();
    t.ok(
        ops::write_file(
            "/System/Kernel/fs/milkfs/restore",
            alloc::format!("v2 {f}").as_bytes(),
        )
        .is_ok()
            && read_str(&f) == "version two, longer",
        "restore via /System/Kernel (path with a space)",
    );
    t.ok(
        snaps().len() == before,
        "no safety snapshot when the current version is kept",
    );
    t.err(
        m::versions(&alloc::format!("{vp}/nope")),
        Errno::ENOENT,
        "versions of a missing file",
    );
    t.err(
        m::versions("/System/Settings/motd"),
        Errno::EXDEV,
        "versions outside the volume",
    );
    for s in snaps() {
        let _ = m::delete_snapshot(&s);
    }
    let _ = ops::unlink(&f);
}

/// check / check --repair on the test volume, with damage made on purpose.
fn milkfs_fsck(
    t: &mut T,
    vol: &alloc::sync::Arc<super::milkfs::Volume>,
    vp: &str,
    dev: &super::blockdev::BlockRef,
) {
    use super::milkfs::{self as m, format::*, items::LIVE};
    t.ok(
        m::check(false).is_ok_and(|r| r.clean()),
        "check: clean volume is clean",
    );
    let _ = ops::write_file(
        &alloc::format!("{vp}/victim.txt"),
        b"precious data, 4 KiB+".repeat(250).as_slice(),
    );
    let _ = ops::write_file(&alloc::format!("{vp}/keep.txt"), b"keep");
    {
        let mut st = vol.state.lock();
        let _ = st.set_cur(LIVE);
        // An inode with a link but no entry, an entry to a missing inode, a wrong count.
        let lost = st.alloc_ino();
        let t0 = (1, 0);
        let _ = st.put_inode(
            lost,
            &InodeItem {
                mode: 0o100644,
                nlink: 1,
                parent: ROOT_INO,
                atime: t0,
                mtime: t0,
                ctime: t0,
                ..InodeItem::default()
            },
        );
        let _ = st.add_dirent(
            ROOT_INO,
            Dirent {
                ino: 999_999,
                dtype: 8,
                name: String::from("ghost"),
            },
        );
        if let Ok(Some(d)) = st.lookup(LIVE, ROOT_INO, "keep.txt") {
            let mut i = st.inode(LIVE, d.ino).unwrap();
            i.nlink = 7;
            let _ = st.put_inode(d.ino, &i);
        }
        let _ = st.commit();
    }
    let gen_slot = vol.state.lock().sb.generation % 2;
    let other = 1 - gen_slot;
    let _ = dev.write_blocks(other * 8, &[0xAB; 4096]);
    let r = m::check(false).unwrap_or_default();
    t.ok(
        r.problems.len() >= 3
            && r.notes.iter().any(|n| n.contains("torn"))
            && r.repaired.is_empty(),
        &alloc::format!(
            "check finds the damage and the torn slot, changes nothing ({} problems)",
            r.problems.len()
        ),
    );
    t.ok(
        read_str(&alloc::format!("{vp}/keep.txt")) == "keep",
        "check alone leaves files alone",
    );
    let r = m::check(true).unwrap_or_default();
    t.ok(
        r.repaired.len() >= 4,
        &alloc::format!("repair fixes it ({} repairs)", r.repaired.len()),
    );
    t.ok(
        m::check(false).is_ok_and(|r| r.clean()),
        "check is clean after repair",
    );
    t.ok(
        ops::readdir(&alloc::format!("{vp}/lost+found"))
            .is_ok_and(|e| e.len() == 1 && e[0].name.starts_with('#')),
        "unreferenced inode is in lost+found",
    );
    t.err(
        ops::stat(&alloc::format!("{vp}/ghost")),
        Errno::ENOENT,
        "dangling entry removed",
    );
    t.ok(
        ops::stat(&alloc::format!("{vp}/keep.txt")).is_ok_and(|s| s.nlink == 1),
        "link count fixed",
    );
    let mut b = [0u8; 4096];
    t.ok(
        dev.read_blocks(other * 8, &mut b).is_ok() && Superblock::decode(&b).is_some(),
        "damaged superblock slot rewritten",
    );
    // Data damage: overwrite a data block behind the cache, then check.
    let blk = {
        let st = vol.state.lock();
        let ino = st
            .lookup(LIVE, ROOT_INO, "victim.txt")
            .ok()
            .flatten()
            .map(|d| d.ino);
        ino.and_then(|i| st.extents(LIVE, i, 0).ok())
            .and_then(|e| e.first().map(|x| x.1.block))
    };
    if let Some(blk) = blk {
        let mut saved = [0u8; 4096];
        let _ = dev.read_blocks(blk * 8, &mut saved);
        let _ = dev.write_blocks(blk * 8, &[0u8; 4096]);
        vol.state.lock().cache().discard();
        let r = m::check(true).unwrap_or_default();
        t.ok(
            r.unrepairable
                .iter()
                .any(|u| u.contains("victim.txt") && u.contains("checksum")),
            "bad data block reported with its path, not 'repaired'",
        );
        t.err(
            ops::read_file(&alloc::format!("{vp}/victim.txt")),
            Errno::EIO,
            "reading it gives EIO",
        );
        let _ = dev.write_blocks(blk * 8, &saved);
        vol.state.lock().cache().discard();
        super::pagecache::drop_all();
        t.ok(
            m::check(false).is_ok_and(|r| r.clean()),
            "clean again once the block is back",
        );
    }
    rm_rf(&alloc::format!("{vp}/lost+found"));
    let _ = ops::unlink(&alloc::format!("{vp}/victim.txt"));
    let _ = ops::unlink(&alloc::format!("{vp}/keep.txt"));
}

/// Small-file benchmark on the test volume: printed, not checked.
/// B+tree deletes: random operations against a model, then the shape after mass deletes.
fn milkfs_tree(t: &mut T, vol: &alloc::sync::Arc<super::milkfs::Volume>, vp: &str) {
    use super::milkfs::{self as m, btree, format::*, items::LIVE};
    use alloc::collections::BTreeMap;
    // Random inserts / replaces / deletes of test items (an unused kind, objects far above
    // any inode) with values of random size, committed now and then; the tree must equal
    // a BTreeMap model throughout and shrink back when everything is deleted.
    const KIND: u8 = 200;
    const OBJ: u64 = 0x7000_0000_0000_0000;
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut model: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    let base = vol
        .state
        .lock()
        .verify_space()
        .map(|s| s.allocated)
        .unwrap_or(0);
    let mut ok = true;
    {
        let mut st = vol.state.lock();
        let _ = st.set_cur(LIVE);
        for i in 0..6000u32 {
            let r = next();
            let key = Key::new(OBJ + r % 400, KIND, (r >> 20) % 50);
            if r >> 40 & 3 == 0 {
                let had = model.remove(&key).is_some();
                ok &= st.test_del(&key).is_ok_and(|f| f == had);
            } else {
                let len = (r >> 44) as usize % 300;
                let v: Vec<u8> = (0..len).map(|j| (j as u64 ^ r) as u8).collect();
                ok &= st.test_put(key, v.clone()).is_ok();
                model.insert(key, v);
            }
            if i % 700 == 0 {
                ok &= st.commit().is_ok();
            }
        }
        let _ = st.commit();
        let got = st.test_items(Key::new(OBJ, KIND, 0), Key::new(OBJ + 400, KIND + 1, 0));
        ok &= got.is_ok_and(|g| g.len() == model.len() && g.into_iter().eq(model.clone()));
        // Delete all but every 20th, check, then the rest.
        let keys: Vec<Key> = model.keys().copied().collect();
        for (i, k) in keys.iter().enumerate() {
            if i % 20 != 0 {
                ok &= st.test_del(k).is_ok_and(|f| f);
                model.remove(k);
            }
        }
        let _ = st.commit();
        let got = st.test_items(Key::new(OBJ, KIND, 0), Key::new(OBJ + 400, KIND + 1, 0));
        ok &= got.is_ok_and(|g| g.into_iter().eq(model.clone()));
        let shape = btree::stats(&st.store, st.sb.root).unwrap_or_default();
        ok &= shape.underfull == 0;
        for k in model.keys() {
            ok &= st.test_del(k).is_ok_and(|f| f);
        }
        let _ = st.commit();
    }
    t.ok(
        ok,
        "B+tree: 6000 random inserts/replaces/deletes match a model, no underfull nodes",
    );
    let after = vol.state.lock().verify_space();
    t.ok(
        after.is_ok_and(|s| s.allocated == base && s.leaked == 0 && s.free_but_used == 0),
        "B+tree: back to the same size after deleting everything, space exact",
    );
    // Mass create / delete through the VFS (scripts/treetest.sh does 50 000).
    match m::tree_test(&alloc::format!("{vp}/tt"), 3000) {
        Ok((_, [s0, s1, s2, s3])) => {
            t.ok(
                s1.fill_pct() >= 70,
                &alloc::format!("3000 files: tree {}% full", s1.fill_pct()),
            );
            t.ok(
                s2.underfull == 0 && s2.height < s1.height && s2.nodes * 10 < s1.nodes,
                &alloc::format!(
                    "99% deleted: height {} -> {}, nodes {} -> {}, none underfull",
                    s1.height,
                    s2.height,
                    s1.nodes,
                    s2.nodes
                ),
            );
            t.ok(
                s3.height == s0.height && s3.nodes == s0.nodes && s3.items == s0.items,
                "all deleted: the tree is as before",
            );
        }
        Err(e) => t.ok(false, &alloc::format!("tree test: {e}")),
    }
}

/// Reflink copies (FICLONE) and dedupe: shared blocks, copy-on-write, exact space
/// through snapshots, rollback and deletes, `check` of the shared-block table.
fn milkfs_reflink(t: &mut T, vol: &alloc::sync::Arc<super::milkfs::Volume>, vp: &str) {
    use super::milkfs::{self as m, format::*, items::LIVE};
    let dir = alloc::format!("{vp}/rl");
    let p = |n: &str| alloc::format!("{dir}/{n}");
    let _ = ops::mkdir(&dir, 0o755);
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 13 % 247) as u8).collect();
    let _ = ops::write_file(&p("src"), &data);
    let _ = super::sync_all();
    let free = || vol.state.lock().store.free_blocks();
    let blocks = |path: &str| ops::stat(path).map(|s| s.blocks).unwrap_or(0);
    let clone = |src: &str, dst: &str| -> super::errno::Result<()> {
        let s = ops::open(src, O_RDONLY, 0)?;
        let d = ops::open(dst, O_WRONLY | O_CREAT, 0o644)?;
        ops::ficlone(&s, &d)
    };
    let f0 = free();
    t.ok(
        clone(&p("src"), &p("copy")).is_ok(),
        "FICLONE a 300 KB file",
    );
    let _ = super::sync_all();
    t.ok(
        ops::read_file(&p("copy")).is_ok_and(|d| d == data),
        "the reflink copy reads the same",
    );
    let used = f0 - free();
    t.ok(
        used < 8 && blocks(&p("copy")) == blocks(&p("src")),
        &alloc::format!("it shares the 74 data blocks ({used} blocks of metadata used)"),
    );
    t.ok(
        vol.state.lock().version == REFLINK_VERSION,
        "the volume is format v2 now (older kernels must refuse it)",
    );
    milk_space(t, vol, "space exact after a reflink");
    // Copy-on-write: writing one copy leaves the other alone.
    if let Ok(f) = ops::open(&p("copy"), O_WRONLY, 0) {
        let _ = f.pwrite(b"changed in the copy", 100_000);
    }
    let mut want = data.clone();
    want[100_000..100_019].copy_from_slice(b"changed in the copy");
    t.ok(
        ops::read_file(&p("copy")).is_ok_and(|d| d == want)
            && ops::read_file(&p("src")).is_ok_and(|d| d == data),
        "writing the copy doesn't touch the original",
    );
    milk_space(t, vol, "space exact after writing into a shared run");
    // A snapshot holds both; delete the original, then the snapshot.
    let _ = m::snapshot("rl-a");
    let _ = ops::unlink(&p("src"));
    t.ok(
        ops::read_file(&p("copy")).is_ok_and(|d| d == want),
        "the copy survives deleting the original",
    );
    milk_space(
        t,
        vol,
        "space exact after deleting the original under a snapshot",
    );
    let _ = m::delete_snapshot("rl-a");
    milk_space(t, vol, "space exact after deleting the snapshot");
    // Reflink after a snapshot, then roll back to it.
    let _ = m::snapshot("rl-b");
    t.ok(
        clone(&p("copy"), &p("copy2")).is_ok(),
        "reflink after a snapshot",
    );
    let _ = ops::unlink(&p("copy"));
    let _ = m::rollback("rl-b");
    t.ok(
        ops::stat(&p("copy2")).is_err() && ops::read_file(&p("copy")).is_ok_and(|d| d == want),
        "rollback: the later reflink is gone, the file is back",
    );
    milk_space(t, vol, "space exact after the rollback");
    let _ = m::delete_snapshot("rl-b");
    // Small (inline) files are just copied; errors.
    let _ = ops::write_file(&p("small"), b"tiny");
    t.ok(
        clone(&p("small"), &p("small2")).is_ok() && read_str(&p("small2")) == "tiny",
        "reflink of an inline file copies it",
    );
    t.err(
        clone(&dir, &p("x")),
        Errno::EISDIR,
        "reflink from a directory",
    );
    t.err(
        clone(&p("small"), &p("small")),
        Errno::EINVAL,
        "reflink onto itself",
    );
    let _ = ops::write_file("/tmp/rl-tmpfs", b"on tmpfs");
    t.err(
        clone("/tmp/rl-tmpfs", &p("y")),
        Errno::EXDEV,
        "reflink from another file system",
    );
    let _ = ops::unlink("/tmp/rl-tmpfs");
    // Dedupe: three identical files written separately, one same-size different one.
    let other: Vec<u8> = data.iter().map(|b| b ^ 1).collect();
    for n in ["d1", "d2", "d3"] {
        let _ = ops::write_file(&p(n), &data);
    }
    let _ = ops::write_file(&p("d4"), &other);
    let _ = super::sync_all();
    let dry = m::dedupe(&dir, true);
    t.ok(
        dry.as_ref()
            .is_ok_and(|r| r.deduped == 2 && r.blocks == 148),
        &alloc::format!("dedupe dry run finds 2 files to share ({dry:?})"),
    );
    let f1 = free();
    let r = m::dedupe(&dir, false);
    t.ok(
        r.as_ref().is_ok_and(|r| r.deduped == 2 && r.freed > 140) && free() > f1 + 140,
        &alloc::format!("dedupe shares them and frees ~148 blocks ({r:?})"),
    );
    t.ok(
        ["d1", "d2", "d3"]
            .iter()
            .all(|n| ops::read_file(&p(n)).is_ok_and(|d| d == data))
            && ops::read_file(&p("d4")).is_ok_and(|d| d == other),
        "deduped files read the same",
    );
    t.ok(
        m::dedupe(&dir, false).is_ok_and(|r| r.already == 2 && r.deduped == 0),
        "dedupe again: already shared",
    );
    milk_space(t, vol, "space exact after dedupe");
    t.ok(
        m::check(false).is_ok_and(|r| r.clean()),
        "check: shared-block table matches the references",
    );
    // Damage the table (one count too low: deleting a copy would free a block in use).
    {
        let mut st = vol.state.lock();
        let _ = st.set_cur(LIVE);
        let b = st
            .lookup(LIVE, st.resolve_in(LIVE, "/rl").unwrap_or(0), "d2")
            .ok()
            .flatten()
            .and_then(|d| st.exts(LIVE, d.ino, 0, 1).ok())
            .and_then(|e| e.first().map(|e| e.block));
        if let Some(b) = b
            && let Ok(ents) = st.bref_entries(LIVE, b, b + 1)
            && let Some(&(s0, l, x)) = ents.first()
        {
            let _ = st.put_bref(s0, l, x - 1);
        }
        let _ = st.commit();
    }
    let found = m::check(false);
    t.ok(
        found
            .as_ref()
            .is_ok_and(|r| r.problems.iter().any(|p| p.contains("shared-block table"))),
        "check finds a wrong shared-block count",
    );
    let fixed = m::check(true);
    t.ok(
        fixed.is_ok_and(|r| r.repaired.iter().any(|x| x.contains("rebuilt")))
            && m::check(false).is_ok_and(|r| r.clean()),
        "repair rebuilds the table",
    );
    // Everything goes; space comes back exactly.
    rm_rf(&dir);
    let _ = super::sync_all();
    milk_space(t, vol, "space exact after deleting every copy");
}

/// snap send / receive between the test volume and a second RAM-disk volume.
fn milkfs_sendrecv(t: &mut T, vp: &str) {
    use super::blockdev::BlockRef;
    use super::milkfs as m;
    const RP: &str = "/Volumes/.milk-recv";
    let src = |n: &str| alloc::format!("{vp}/sr/{n}");
    let _ = ops::mkdir(&alloc::format!("{vp}/sr"), 0o755);
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 241) as u8).collect();
    let _ = ops::write_file(&src("big"), &big);
    if let (Ok(s), Ok(d)) = (
        ops::open(&src("big"), O_RDONLY, 0),
        ops::open(&src("copy"), O_WRONLY | O_CREAT, 0o644),
    ) {
        let _ = ops::ficlone(&s, &d);
    }
    let _ = ops::write_file(&src("small"), b"small one");
    let _ = ops::symlink("small", &src("link"));
    let _ = m::snapshot("sr1");
    let _ = ops::write_file(&src("small2"), b"another");
    let _ = ops::unlink(&src("small"));
    if let Ok(f) = ops::open(&src("big"), O_WRONLY, 0) {
        let _ = f.pwrite(b"changed!", 100_000);
    }
    let _ = m::snapshot("sr2");
    let _ = ops::write_file(&src("third"), b"third snapshot");
    let _ = m::snapshot("sr3");
    t.ok(
        m::send("sr1", None, "/tmp/sr1.stream", m::StreamCrypt::Default).is_ok()
            && m::send(
                "sr2",
                Some("sr1"),
                "/tmp/sr2.stream",
                m::StreamCrypt::Default,
            )
            .is_ok()
            && m::send(
                "sr3",
                Some("sr2"),
                "/tmp/sr3.stream",
                m::StreamCrypt::Default,
            )
            .is_ok(),
        "send sr1 (full), sr2 and sr3 (incremental)",
    );
    t.err(
        m::send("sr1", Some("sr2"), "/tmp/x.stream", m::StreamCrypt::Default),
        Errno::EINVAL,
        "send: the base must be older",
    );
    // The target: a fresh volume on its own RAM disk.
    let Some(disk) = t.must(
        super::ramdisk::RamDisk::new("ram2", 16 << 20),
        "receive RAM disk",
    ) else {
        return;
    };
    let dev: BlockRef = alloc::sync::Arc::new(disk);
    let _ = m::mkfs(&dev, "RecvTest", false);
    let Some((tv, _)) = t.must(m::mount_secondary(dev.clone(), RP), "mount the target") else {
        return;
    };
    let snaps = |v: &m::Volume| {
        v.state
            .lock()
            .snap_list()
            .into_iter()
            .map(|s| s.name)
            .collect::<Vec<_>>()
    };
    t.err(
        m::receive("/tmp/sr2.stream", Some(RP), false, None),
        Errno::ENOENT,
        "receive: an incremental whose base is missing is refused",
    );
    t.ok(
        m::receive("/tmp/sr1.stream", Some(RP), false, None).is_ok()
            && m::receive("/tmp/sr2.stream", Some(RP), false, None).is_ok(),
        "receive sr1, then sr2 on top",
    );
    let same = |snap: &str, n: &str| {
        let a = ops::read_file(&alloc::format!("{vp}/.snapshots/{snap}/sr/{n}"));
        let b = ops::read_file(&alloc::format!("{RP}/.snapshots/{snap}/sr/{n}"));
        a.is_ok() && a == b
    };
    t.ok(
        same("sr1", "big")
            && same("sr1", "copy")
            && same("sr1", "small")
            && same("sr2", "big")
            && same("sr2", "small2")
            && ops::readlink(&alloc::format!("{RP}/.snapshots/sr1/sr/link")).as_deref()
                == Ok("small")
            && ops::stat(&alloc::format!("{RP}/.snapshots/sr2/sr/small")).is_err()
            && ops::read_file(&alloc::format!("{RP}/sr/big"))
                == ops::read_file(&alloc::format!("{vp}/.snapshots/sr2/sr/big")),
        "received snapshots and the live tree match the source",
    );
    t.ok(
        snaps(&tv) == ["sr1", "sr2"],
        "the target has exactly sr1 and sr2",
    );
    t.err(
        m::receive("/tmp/sr2.stream", Some(RP), false, None),
        Errno::EEXIST,
        "receiving sr2 again is refused",
    );
    // A damaged stream: refused, nothing changes.
    let mut bad = ops::read_file("/tmp/sr3.stream").unwrap_or_default();
    let mid = bad.len() / 2;
    if let Some(b) = bad.get_mut(mid) {
        *b ^= 0x10;
    }
    let _ = ops::write_file("/tmp/bad.stream", &bad);
    let before = tv.state.lock().verify_space().map(|s| s.allocated);
    t.err(
        m::receive("/tmp/bad.stream", Some(RP), false, None),
        Errno::EIO,
        "a damaged stream is refused",
    );
    let after = tv.state.lock().verify_space();
    t.ok(
        snaps(&tv) == ["sr1", "sr2"]
            && tv.state.lock().clone_list().is_empty()
            && after
                .is_ok_and(|s| s.leaked == 0 && s.free_but_used == 0 && Ok(s.allocated) == before),
        "and the target is exactly as before (no clone left, space exact)",
    );
    // Written since the base: refused, unless forced (rolled back to sr2 first).
    let _ = ops::write_file(&alloc::format!("{RP}/sr/local"), b"local change");
    t.err(
        m::receive("/tmp/sr3.stream", Some(RP), false, None),
        Errno::EBUSY,
        "target changed since the base: refused",
    );
    t.ok(
        m::receive("/tmp/sr3.stream", Some(RP), true, None).is_ok()
            && ops::stat(&alloc::format!("{RP}/sr/local")).is_err()
            && read_str(&alloc::format!("{RP}/sr/third")) == "third snapshot",
        "--force: rolled back to sr2, then sr3 received",
    );
    let r = tv.state.lock().check(false);
    t.ok(
        r.is_ok_and(|r| r.clean()),
        "target check clean (shared-block counts too)",
    );
    for s in ["sr1", "sr2", "sr3"] {
        let _ = tv.state.lock().snap_delete(s);
    }
    let sp = tv.state.lock().verify_space();
    t.ok(
        sp.is_ok_and(|s| s.leaked == 0 && s.free_but_used == 0),
        "target space exact after deleting the received snapshots",
    );
    let _ = super::unmount(RP);
    drop(tv);
    milkfs_sendrecv_sealed(t, vp, &dev, RP);
    let _ = ops::rmdir(RP);
    drop(dev);
    for s in ["sr1", "sr2", "sr3"] {
        let _ = m::delete_snapshot(s);
    }
    rm_rf(&alloc::format!("{vp}/sr"));
    for f in [
        "sr1", "sr2", "sr3", "bad", "x", "sr1x", "sr2x", "badx", "shortx",
    ] {
        let _ = ops::unlink(&alloc::format!("/tmp/{f}.stream"));
    }
}

/// Encrypted streams: sealed on the way out (no plaintext in the file), refused without
/// or with a wrong passphrase, refused when altered or cut short, received with the
/// right one (onto `dev`, formatted again, at `rp`).
fn milkfs_sendrecv_sealed(t: &mut T, vp: &str, dev: &super::blockdev::BlockRef, rp: &str) {
    use super::milkfs as m;
    const PASS: &str = "fake-stream-passphrase";
    let enc = |p| m::StreamCrypt::Passphrase(p);
    t.ok(
        m::send("sr1", None, "/tmp/sr1x.stream", enc(PASS)).is_ok_and(|r| r.encrypted)
            && m::send("sr2", Some("sr1"), "/tmp/sr2x.stream", enc(PASS)).is_ok(),
        "send sr1 and sr2 encrypted",
    );
    t.err(
        m::send("sr1", None, "/tmp/x.stream", enc("short")),
        Errno::EINVAL,
        "a stream passphrase under 8 characters is refused",
    );
    let raw = ops::read_file("/tmp/sr1x.stream").unwrap_or_default();
    let has = |n: &[u8]| raw.windows(n.len()).any(|w| w == n);
    t.ok(
        raw.starts_with(b"MILKSTRX") && !has(b"small one") && !has(b"MILKSTRM") && !has(b"sr1"),
        "the encrypted stream has no plaintext (names, file contents, inner header)",
    );
    if m::mkfs(dev, "RecvSealed", true).is_err() {
        t.ok(false, "mkfs the sealed-receive target");
        return;
    }
    let Some((tv, _)) = t.must(m::mount_secondary(dev.clone(), rp), "mount it") else {
        return;
    };
    t.err(
        m::receive("/tmp/sr1x.stream", Some(rp), false, None),
        Errno::EACCES,
        "an encrypted stream without its passphrase is refused",
    );
    t.err(
        m::receive(
            "/tmp/sr1x.stream",
            Some(rp),
            false,
            Some("wrong-fake-passphrase"),
        ),
        Errno::EACCES,
        "a wrong stream passphrase is refused",
    );
    let mut bad = raw.clone();
    let mid = bad.len() / 2;
    if let Some(b) = bad.get_mut(mid) {
        *b ^= 1;
    }
    let _ = ops::write_file("/tmp/badx.stream", &bad);
    let _ = ops::write_file("/tmp/shortx.stream", &raw[..raw.len().saturating_sub(40)]);
    t.err(
        m::receive("/tmp/badx.stream", Some(rp), false, Some(PASS)),
        Errno::EIO,
        "an altered encrypted stream is refused",
    );
    t.err(
        m::receive("/tmp/shortx.stream", Some(rp), false, Some(PASS)),
        Errno::EIO,
        "a cut-short encrypted stream is refused",
    );
    t.ok(
        tv.state.lock().snap_list().is_empty(),
        "and nothing was received",
    );
    t.ok(
        m::receive("/tmp/sr1x.stream", Some(rp), false, Some(PASS)).is_ok_and(|r| r.encrypted)
            && m::receive("/tmp/sr2x.stream", Some(rp), false, Some(PASS)).is_ok(),
        "the right passphrase: sr1, then sr2 received",
    );
    let a = ops::read_file(&alloc::format!("{vp}/.snapshots/sr2/sr/big"));
    let b = ops::read_file(&alloc::format!("{rp}/.snapshots/sr2/sr/big"));
    t.ok(
        a.is_ok() && a == b,
        "received through encryption: same contents",
    );
    let r = tv.state.lock().check(false);
    t.ok(
        r.is_ok_and(|r| r.clean()),
        "sealed-receive target check clean",
    );
    let _ = super::unmount(rp);
}

/// Scrub: damage a live file's block and a snapshot-only file's block behind every cache,
/// scrub, and see both reported with their paths. And the hold-back of frees.
fn milkfs_scrub(
    t: &mut T,
    vol: &alloc::sync::Arc<super::milkfs::Volume>,
    vp: &str,
    dev: &super::blockdev::BlockRef,
) {
    use super::milkfs::{self as m, items::LIVE, scrub};
    let d = alloc::format!("{vp}/scrub");
    let _ = ops::mkdir(&d, 0o755);
    let body: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    let _ = ops::write_file(&alloc::format!("{d}/a"), &body);
    let _ = ops::write_file(&alloc::format!("{d}/b"), &body);
    let _ = m::snapshot("sc1");
    let _ = ops::unlink(&alloc::format!("{d}/b"));
    let _ = super::sync_all();
    let before = scrub::scrub_volume(vol, vp, "test", false);
    t.ok(before.is_ok(), "scrub runs");
    let n0 = before.map(|e| e.len()).unwrap_or(0);
    // Damage the first data block of a (live) and of b (only in sc1).
    let first = |view: u64, name: &str| {
        let st = vol.state.lock();
        let dir = st.resolve_in(view, "/scrub")?;
        let ino = st.lookup(view, dir, name).ok()??.ino;
        st.extents(view, ino, 0).ok()?.first().map(|x| x.1.block)
    };
    let snap_gen = vol
        .state
        .lock()
        .snaps
        .iter()
        .find(|s| s.name == "sc1")
        .map(|s| s.generation)
        .unwrap_or(0);
    for b in [first(LIVE, "a"), first(snap_gen, "b")]
        .into_iter()
        .flatten()
    {
        let mut blk = [0u8; 4096];
        let _ = dev.read_blocks(b * 8, &mut blk);
        blk[100] ^= 0x01; // one bit
        let _ = dev.write_blocks(b * 8, &blk);
    }
    let errs = scrub::scrub_volume(vol, vp, "test", false).unwrap_or_default();
    let has = |s: &str| errs.iter().any(|e| e.what.contains(s));
    t.ok(
        errs.len() == n0 + 2
            && has(&alloc::format!(
                "data of {d}/a at offset 0 (live, snapshot sc1)"
            ))
            && has(&alloc::format!("data of {d}/b at offset 0 (snapshot sc1)")),
        &alloc::format!("scrub finds both flipped bits, with paths ({errs:?})"),
    );
    // Frees while a scrub holds them: not reused, space exact, released after.
    {
        let mut st = vol.state.lock();
        st.store.hold_frees();
    }
    let free0 = vol.state.lock().store.free_blocks();
    let _ = ops::unlink(&alloc::format!("{d}/a"));
    let _ = m::delete_snapshot("sc1");
    let _ = super::sync_all();
    let (held, free1, sp) = {
        let mut st = vol.state.lock();
        let sp = st.verify_space();
        (st.store.held_count(), st.store.free_blocks(), sp)
    };
    t.ok(
        held >= 4 && free1 <= free0 && sp.is_ok_and(|s| s.leaked == 0 && s.free_but_used == 0),
        &alloc::format!("frees during a scrub are held ({held} blocks), space still exact"),
    );
    vol.state.lock().store.release_held();
    t.ok(
        vol.state.lock().store.free_blocks() >= free1 + held,
        "released when it ends",
    );
    milk_space(t, vol, "space exact after the scrub");
    rm_rf(&d);
}

/// A file made straight in a volume's live tree (no VFS): returns its inode.
fn raw_file(
    st: &mut super::milkfs::volume::State,
    name: &str,
    data: &[u8],
    nlink: u32,
) -> Option<u64> {
    use super::milkfs::format::*;
    let _ = st.set_cur(super::milkfs::items::LIVE);
    let ino = st.alloc_ino();
    let size = st.write_data(ino, 0, 0, data).ok()?;
    st.put_inode(
        ino,
        &InodeItem {
            mode: 0o100644,
            nlink,
            size,
            parent: ROOT_INO,
            ..InodeItem::default()
        },
    )
    .ok()?;
    if nlink > 0 {
        st.add_dirent(
            ROOT_INO,
            Dirent {
                ino,
                dtype: 8,
                name: String::from(name),
            },
        )
        .ok()?;
    }
    Some(ino)
}

/// The persistent space map, on its own RAM disk driven through `State` (re-opens are
/// "reboots"; dropping a State without a commit is a crash).
fn milkfs_spacemap(t: &mut T) {
    use super::blockdev::BlockRef;
    use super::milkfs::format::*;
    use super::milkfs::items::LIVE;
    use super::milkfs::volume::State;
    let Some(disk) = t.must(
        super::ramdisk::RamDisk::new("ram1", 8 << 20),
        "map RAM disk",
    ) else {
        return;
    };
    let dev: BlockRef = alloc::sync::Arc::new(disk);
    if super::milkfs::mkfs(&dev, "MapTest", false).is_err() {
        t.ok(false, "mkfs the map test volume");
        return;
    }
    let exact = |st: &mut State| {
        st.verify_space()
            .is_ok_and(|s| s.leaked == 0 && s.free_but_used == 0 && s.reachable == s.allocated)
    };
    let Ok((mut st, r)) = State::open(dev.clone()) else {
        t.ok(false, "open the map test volume");
        return;
    };
    t.ok(!r.fast, "fresh volume: no space map yet, scanned");
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let _ = raw_file(&mut st, "big", &big, 1);
    let _ = raw_file(&mut st, "small", b"small", 1);
    let _ = st.commit();
    // Unlinked while open, then the machine goes down: on the orphan list.
    let orphan = raw_file(&mut st, "", &big[..50_000], 0).unwrap_or(0);
    let _ = st.add_orphan(orphan);
    let _ = st.commit();
    let committed = st.sb.generation;
    drop(st); // crash: no further commit

    let Ok((mut st, r)) = State::open(dev.clone()) else {
        t.ok(false, "re-open with a space map");
        return;
    };
    t.ok(
        r.fast && r.generation == committed,
        &alloc::format!(
            "re-open loads the space map, no scan ({} us)",
            r.mount_ns / 1000
        ),
    );
    t.ok(
        r.orphans == 1 && st.inode(LIVE, orphan).is_err(),
        "orphan list: the unlinked-while-open file is deleted at mount",
    );
    t.ok(exact(&mut st), "space exact after a mapped mount");
    let mut b = vec![0u8; 300_000];
    t.ok(
        st.lookup(LIVE, ROOT_INO, "big")
            .ok()
            .flatten()
            .and_then(|d| st.read_data(LIVE, d.ino, 300_000, 0, &mut b).ok())
            == Some(300_000)
            && b == big,
        "data intact",
    );

    // Damage a bitmap chunk: mount notices (checksum) and rebuilds from the trees.
    let map = st.sb.spacemap;
    let chunk = st
        .store
        .read_map_index(map)
        .ok()
        .and_then(|i| i.chunks.first().copied());
    drop(st);
    if let Some(c) = chunk {
        let _ = dev.write_blocks(c.block * 8, &[0x5au8; BLOCK]);
    }
    let Ok((mut st, r)) = State::open(dev.clone()) else {
        t.ok(false, "re-open with a damaged space map");
        return;
    };
    t.ok(
        !r.fast && r.problems == 0,
        "damaged space map: rebuilt by a scan",
    );
    t.ok(exact(&mut st), "space exact after the rebuild");
    let _ = raw_file(&mut st, "after", b"x", 1);
    let _ = st.commit();
    drop(st);
    let r = State::open(dev.clone()).map(|(_, r)| r.fast);
    t.ok(r == Ok(true), "the next commit writes a good map again");

    // A free map that lost a block in use: check finds it, repair marks it again.
    let Ok((mut st, _)) = State::open(dev.clone()) else {
        return;
    };
    let victim = st
        .lookup(LIVE, ROOT_INO, "big")
        .ok()
        .flatten()
        .and_then(|d| st.extents(LIVE, d.ino, 0).ok())
        .and_then(|e| e.first().map(|x| x.1.block));
    if let Some(v) = victim {
        st.store.sweep_free(v);
    }
    let found = st.check(false).is_ok_and(|r| {
        r.problems
            .iter()
            .any(|p| p.contains("1 reachable blocks marked free"))
    });
    t.ok(found, "check finds a block in use marked free");
    let fixed = st
        .check(true)
        .is_ok_and(|r| r.repaired.iter().any(|x| x.contains("marked 1")));
    t.ok(fixed && exact(&mut st), "repair marks it used again");
}

fn milkfs_bench(v: &super::milkfs::Volume, vp: &str) {
    let dir = alloc::format!("{vp}/.bench");
    rm_rf(&dir);
    let _ = super::sync_all();
    let commits = || v.state.lock().commits;
    let (c0, t0) = (commits(), crate::timer::now_ns());
    let _ = ops::mkdir(&dir, 0o755);
    let data = [0x6du8; 1024];
    for i in 0..200 {
        let _ = ops::write_file(&alloc::format!("{dir}/f{i}"), &data);
    }
    let _ = super::sync_all();
    let (c1, t1) = (commits(), crate::timer::now_ns());
    let mut ok = 0;
    for i in 0..200 {
        if ops::read_file(&alloc::format!("{dir}/f{i}")).is_ok_and(|d| d == data) {
            ok += 1;
        }
    }
    let t2 = crate::timer::now_ns();
    rm_rf(&dir);
    let _ = super::sync_all();
    let (c3, t3) = (commits(), crate::timer::now_ns());
    kprintln!(
        "milkfs bench: 200 x 1 KiB files: create {} ms ({} commits), read {} ms ({ok} ok), delete {} ms ({} commits)",
        (t1 - t0) / 1_000_000,
        c1 - c0,
        (t2 - t1) / 1_000_000,
        (t3 - t2) / 1_000_000,
        c3 - c1
    );
}

/// Print a few inspectable files so they show up in the boot log.
fn show(path: &str) {
    kprintln!("$ text show {path}");
    kprintln!("{}", read_str(path).trim_end());
}

/// The crypto this file system uses: the shared crate's vectors for XChaCha20-Poly1305,
/// HMAC/PBKDF2 and scrypt (also printed by `crypto::init`), and scrypt's RFC 7914 vector
/// once more through the kernel's page-frame scratch, timed.
fn crypto(t: &mut T) {
    for (name, p, n) in milkcrypto::selftest() {
        if matches!(
            name,
            "xchacha20poly1305" | "hmac-sha256+pbkdf2" | "scrypt" | "chacha20" | "poly1305"
        ) {
            t.ok(p == n, &alloc::format!("crypto: {name} {p}/{n} vectors"));
        }
    }
    let t0 = crate::timer::now_ns();
    let mut out = [0u8; 64];
    let ok = crate::crypto::frames::scrypt(b"password", b"NaCl", 10, 8, 16, &mut out).is_some()
        && out[..]
            == milkcrypto::unhex_vec(
                "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640",
            )[..];
    t.ok(
        ok,
        &alloc::format!(
            "crypto: scrypt RFC 7914 vector with the array in page frames ({} ms)",
            (crate::timer::now_ns() - t0) / 1_000_000
        ),
    );
}

/// fs::notify events, and the search index over a small tmpfs tree (built and updated
/// synchronously here; the real indexer thread starts later, on /Users/milk).
fn notify_and_search(t: &mut T) {
    use super::notify::{self, Event};
    use super::search;
    const R: &str = "/tmp/st";
    let p = |n: &str| alloc::format!("{R}/{n}");
    rm_rf(R);
    let _ = ops::mkdir(R, 0o755);
    // Events, in order, for a little sequence of changes.
    let w = notify::watch(64);
    let _ = ops::mkdir(&p("d"), 0o755);
    let _ = ops::write_file(&p("d/a.txt"), b"x");
    let _ = ops::rename(&p("d/a.txt"), &p("d/b.txt"));
    let _ = ops::unlink(&p("d/b.txt"));
    let mut kinds = Vec::new();
    while let Some(e) = w.next(0) {
        kinds.push(match e {
            Event::Created { name, .. } => alloc::format!("+{name}"),
            Event::Removed { name, .. } => alloc::format!("-{name}"),
            Event::Renamed { from, to, .. } => alloc::format!("{from}>{to}"),
            Event::Modified { .. } => String::from("M"),
            Event::Attr { .. } => String::from("A"),
            _ => String::from("?"),
        });
    }
    t.ok(
        kinds == ["+d", "+a.txt", "M", "M", "a.txt>b.txt", "-b.txt"],
        &alloc::format!("notify: events in order ({kinds:?})"),
    );
    let small = notify::watch(16);
    for i in 0..40 {
        let _ = ops::write_file(&p(&alloc::format!("d/f{i}")), b"");
    }
    t.ok(
        small.next(0) == Some(Event::Overflow),
        "notify: a full queue reports Overflow",
    );
    drop(small);
    rm_rf(&p("d"));
    // A little tree.
    let _ = ops::mkdir(&p("Documents"), 0o755);
    let _ = ops::mkdir(&p("code"), 0o755);
    let _ = ops::mkdir(&p(".hidden"), 0o755);
    let _ = ops::write_file(
        &p("Documents/Project Plan.md"),
        b"# Roadmap\nThe milkshake launch, then pasture time.",
    );
    let _ = ops::write_file(&p("Documents/notes.txt"), b"buy oat milk; call the vet");
    let _ = ops::write_file(
        &p("code/main.rs"),
        b"fn main() { println!(\"hello pasture\"); }",
    );
    let _ = ops::write_file(&p("code/readme"), b"pasture notes, no extension");
    let _ = ops::write_file(&p("pic.png"), b"\x89PNG\r\n\x1a\n....");
    let _ = ops::write_file(&p("blob"), &[0u8, 1, 2, 3, 255, 254]);
    let _ = ops::write_file(&p(".hidden/secret.txt"), b"pasture");
    search::build_now(&[R]);
    let w = notify::watch(256);
    let q = |s: &str| -> Vec<String> {
        search::query(s, 0)
            .map(|h| {
                h.into_iter()
                    .map(|h| String::from(&h.path[R.len()..]))
                    .collect()
            })
            .unwrap_or_default()
    };
    t.ok(
        search::status().files == 6 && search::status().folders == 2,
        &alloc::format!(
            "search: 6 files, 2 folders (hidden ones skipped): {:?}",
            search::status()
        ),
    );
    t.ok(
        q("pasture")
            == [
                "/Documents/Project Plan.md",
                "/code/main.rs",
                "/code/readme",
            ]
            || {
                let mut v = q("pasture");
                v.sort();
                v == [
                    "/Documents/Project Plan.md",
                    "/code/main.rs",
                    "/code/readme",
                ]
            },
        &alloc::format!(
            "search: content word, hidden file not found ({:?})",
            q("pasture")
        ),
    );
    t.ok(
        q("proj") == ["/Documents/Project Plan.md"],
        "search: name prefix",
    );
    t.ok(
        q("notes").first().map(String::as_str) == Some("/Documents/notes.txt")
            && q("notes").contains(&String::from("/code/readme")),
        &alloc::format!(
            "search: a name match ranks above a content match ({:?})",
            q("notes")
        ),
    );
    t.ok(
        q("milk").len() == 2,
        &alloc::format!(
            "search: word and word prefix (milk, milkshake): {:?}",
            q("milk")
        ),
    );
    t.ok(
        q("pasture kind:md") == ["/Documents/Project Plan.md"]
            && q("kind:png") == ["/pic.png"]
            && q("kind:data") == ["/blob"]
            && q("kind:text pasture").len() == 3,
        "search: kind filters (extension, magic, sniffed text)",
    );
    let mut docs = q("in:Documents");
    docs.sort();
    t.ok(
        docs == ["/Documents/Project Plan.md", "/Documents/notes.txt"],
        &alloc::format!("search: in:Documents ({docs:?})"),
    );
    t.ok(
        q("vet modified:today") == ["/Documents/notes.txt"] && q("vet modified:1h").len() == 1,
        "search: modified: filters",
    );
    t.ok(
        q("oat milk") == ["/Documents/notes.txt"],
        "search: all words must match",
    );
    // Incremental: create, rename, delete, a new folder, a renamed folder.
    let _ = ops::write_file(&p("Documents/zebra.txt"), b"a zebra crossing");
    let _ = ops::rename(&p("Documents/notes.txt"), &p("Documents/old-notes.txt"));
    let _ = ops::unlink(&p("pic.png"));
    let _ = ops::mkdir(&p("code/sub"), 0o755);
    let _ = ops::write_file(&p("code/sub/deep.md"), b"deep pasture");
    let _ = ops::rename(&p("code"), &p("src"));
    if let Ok(f) = ops::open(&p("Documents/old-notes.txt"), O_WRONLY, 0) {
        let _ = f.pwrite(b"sell", 0); // "buy" -> "sell"
    }
    search::apply_now(&w);
    let mut past = q("pasture");
    past.sort();
    t.ok(
        q("zebra") == ["/Documents/zebra.txt"]
            && q("old") == ["/Documents/old-notes.txt"]
            && q("kind:png").is_empty()
            && past == ["/Documents/Project Plan.md", "/src/main.rs", "/src/readme", "/src/sub/deep.md"]
            && q("sell") == ["/Documents/old-notes.txt"]
            && q("buy").is_empty(),
        &alloc::format!(
            "search: create, rename, delete, folder rename and a rewrite are all reflected ({past:?})"
        ),
    );
    t.ok(search::roundtrip_ok(), "search: the saved form round-trips");
    drop(w);
    search::reset();
    rm_rf(R);
}

/// `/System/Kernel/fs/milkfs` controls (on the test volume): the errno of a bad write, and
/// `/System/Kernel/fs/milkfs/error` saying why; the authorisation hook for destructive ones.
fn milkfs_control_errors(t: &mut T) {
    let ctl = |name: &str, arg: &str| {
        ops::write_file(
            &alloc::format!("/System/Kernel/fs/milkfs/{name}"),
            arg.as_bytes(),
        )
    };
    let error = || read_str("/System/Kernel/fs/milkfs/error");
    use super::milkfs::label_ok;
    t.ok(
        label_ok("Milk")
            && label_ok("My Backups (2026)")
            && label_ok("Böhm")
            && label_ok(&"x".repeat(31))
            && !label_ok("")
            && !label_ok(&"x".repeat(32))
            && !label_ok(&"é".repeat(16))
            && !label_ok("a/b")
            && !label_ok(".hidden")
            && !label_ok("..")
            && !label_ok("tab\there"),
        "volume labels: 1-31 bytes of UTF-8, no '/', no control characters, no leading '.'",
    );
    let cases: [(&str, &str, Errno, &str); 9] = [
        (
            "snapshot",
            "bad/name",
            Errno::EINVAL,
            "snapshot: EINVAL: names are letters",
        ),
        ("snapshot", "  \n", Errno::EINVAL, "snapshot: EINVAL: "),
        (
            "delete",
            "no-such-snap",
            Errno::ENOENT,
            "delete: ENOENT: no such snapshot",
        ),
        (
            "rollback",
            "no-such-snap",
            Errno::ENOENT,
            "rollback: ENOENT: no such snapshot",
        ),
        (
            "clone",
            "no-such-snap c1",
            Errno::ENOENT,
            "clone: ENOENT: no such snapshot",
        ),
        (
            "discard",
            "no-such-clone",
            Errno::ENOENT,
            "discard: ENOENT: no such clone",
        ),
        (
            "mkfs",
            "nodev0",
            Errno::ENODEV,
            "mkfs: ENODEV: no such device",
        ),
        (
            "lock",
            "nodev0",
            Errno::ENODEV,
            "lock: ENODEV: no mounted milkfs volume nodev0",
        ),
        (
            "passwd",
            "old-fake-pass\nnew-fake-pass",
            Errno::EINVAL,
            "passwd: EINVAL: MilkTest isn't encrypted",
        ),
    ];
    for (name, arg, want, text) in cases {
        t.err(
            ctl(name, arg),
            want,
            &alloc::format!("control {name} {arg:?}"),
        );
        let e = error();
        t.ok(
            e.starts_with(text) && e.ends_with('\n'),
            &alloc::format!("error file after {name} {arg:?}: {e:?}, want {text:?}..."),
        );
    }
    t.ok(
        ctl("sync", "now").is_ok() && error().is_empty(),
        "a write that works clears the error file",
    );
    // The snapshot + clone limit has its own reason (not "volume full").
    let mut made = 0;
    let full = loop {
        match super::milkfs::snapshot(&alloc::format!("lim-{made}")) {
            Ok(_) => made += 1,
            Err(e) => break e,
        }
        if made > 40 {
            break Errno::EINVAL;
        }
    };
    t.ok(full == Errno::ENOSPC, "snapshots stop at the record limit");
    t.err(
        ctl("snapshot", "one-too-many"),
        Errno::ENOSPC,
        "snapshot at the limit",
    );
    t.ok(
        error().starts_with("snapshot: ENOSPC: 30 snapshots and clones at most"),
        &alloc::format!("limit reason: {:?}", error()),
    );
    for i in 0..made {
        let _ = super::milkfs::delete_snapshot(&alloc::format!("lim-{i}"));
    }
    // The authorisation hook: refuses destructive controls only, with its reason.
    fn deny(op: &str) -> Result<(), Errno> {
        super::milkfs::set_reason(&alloc::format!("{op} needs an admin ticket (selftest)"));
        Err(Errno::EACCES)
    }
    let prev = super::milkfs::set_authorizer(Some(deny));
    t.err(
        ctl("delete", "whatever"),
        Errno::EACCES,
        "hook refuses delete",
    );
    t.ok(
        error() == "delete: EACCES: delete needs an admin ticket (selftest)\n",
        &alloc::format!("hook's reason: {:?}", error()),
    );
    t.err(
        ctl("check", "repair"),
        Errno::EACCES,
        "hook refuses check repair",
    );
    t.ok(
        error().contains("check repair needs"),
        "hook sees `check repair`",
    );
    t.ok(
        ctl("check", "check").is_ok(),
        "hook leaves a plain check alone",
    );
    t.ok(
        ctl("snapshot", "hook-ok").is_ok() && super::milkfs::delete_snapshot("hook-ok").is_ok(),
        "hook leaves snapshot alone",
    );
    super::milkfs::set_authorizer(prev);
    t.err(
        ctl("delete", "whatever"),
        Errno::ENOENT,
        "hook removed: delete runs again",
    );
}

/// Scrub covers an encrypted volume's keyring: a damaged keyring block is reported (it
/// is plaintext, checked by xxh64 and by decoding it), a good one isn't.
fn milkfs_scrub_keyring(t: &mut T) {
    use super::blockdev::BlockRef;
    use super::milkfs as m;
    const VP: &str = "/Volumes/.milk-keyring";
    let Some(disk) = t.must(
        super::ramdisk::RamDisk::new("ram3", 8 << 20),
        "keyring RAM disk",
    ) else {
        return;
    };
    let dev: BlockRef = alloc::sync::Arc::new(disk);
    let Some(Some(rec)) = t.must(
        m::volume::mkfs_encrypted(&dev, "KeyTest", "fake-selftest-passphrase", true),
        "mkfs an encrypted test volume",
    ) else {
        return;
    };
    let Some((v, _)) = t.must(
        m::mount_encrypted_secondary(dev.clone(), VP, &rec),
        "mount it with the recovery key",
    ) else {
        return;
    };
    let _ = ops::write_file(&alloc::format!("{VP}/f"), b"some data");
    let _ = v.state.lock().commit();
    let clean = m::scrub::scrub_volume(&v, VP, "test", false);
    t.ok(
        clean.as_ref().is_ok_and(|e| e.is_empty()),
        &alloc::format!("scrub of an encrypted volume: clean ({clean:?})"),
    );
    let kr = v.state.lock().keyring;
    let per = (crate::fs::milkfs::format::BLOCK / dev.block_size()) as u64;
    let mut b = alloc::vec![0u8; crate::fs::milkfs::format::BLOCK];
    let _ = dev.read_blocks(kr.block * per, &mut b);
    let good = b.clone();
    b[100] ^= 0x01; // inside a slot's salt
    let _ = dev.write_blocks(kr.block * per, &b);
    let bad = m::scrub::scrub_volume(&v, VP, "test", false);
    t.ok(
        bad.as_ref()
            .is_ok_and(|e| e.len() == 1 && e[0].block == kr.block && e[0].what.contains("keyring")),
        &alloc::format!("scrub reports a damaged keyring block ({bad:?})"),
    );
    let _ = dev.write_blocks(kr.block * per, &good);
    let again = m::scrub::scrub_volume(&v, VP, "test", false);
    t.ok(
        again.is_ok_and(|e| e.is_empty()),
        "and is clean again once it is put back",
    );
    let _ = super::unmount(VP);
    let _ = ops::rmdir(VP);
}

/// statfs on the RAM file systems, and the Linux `struct statfs` layout.
fn statfs_basics(t: &mut T) {
    use super::node::{self as n, StatFs};
    let tmp = ops::statfs("/tmp");
    t.ok(
        tmp.as_ref().is_ok_and(|s| {
            s.fs_type == n::TMPFS_MAGIC && s.bsize == 4096 && s.blocks > 0 && s.bfree <= s.blocks
        }),
        &alloc::format!("statfs /tmp: tmpfs with a size ({tmp:?})"),
    );
    for (p, magic) in [
        ("/proc", n::PROC_MAGIC),
        ("/System/Kernel/fs", n::SYSFS_MAGIC),
        ("/dev/null", n::DEVFS_MAGIC),
    ] {
        let s = ops::statfs(p);
        t.ok(
            s.as_ref()
                .is_ok_and(|s| s.fs_type == magic && s.blocks == 0 && s.bfree == 0),
            &alloc::format!("statfs {p}: its type, no sizes ({s:?})"),
        );
    }
    t.err(
        ops::statfs("/tmp/no/such/thing"),
        Errno::ENOENT,
        "statfs of a missing path",
    );
    let s = StatFs {
        fs_type: 1,
        bsize: 2,
        blocks: 3,
        bfree: 4,
        bavail: 5,
        files: 6,
        ffree: 7,
        fsid: 0x0000_0009_0000_0008,
        namelen: 10,
        flags: 11,
    };
    let l = s.to_linux();
    let b = l.as_bytes();
    let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    let fsid = (
        i32::from_le_bytes(b[56..60].try_into().unwrap()),
        i32::from_le_bytes(b[60..64].try_into().unwrap()),
    );
    t.ok(
        (0..7).all(|i| w(i) == i as u64 + 1)
            && fsid == (8, 9)
            && w(8) == 10
            && w(9) == 2
            && w(10) == 11
            && (11..15).all(|i| w(i) == 0),
        "struct statfs layout (120 bytes, f_frsize = f_bsize)",
    );
}

/// statfs on the milkfs test volume: sizes, space going and coming back, snapshots
/// read-only.
fn milkfs_statfs(t: &mut T, vp: &str) {
    use super::node as n;
    let Some(s0) = t.must(ops::statfs(vp), "statfs the test volume") else {
        return;
    };
    t.ok(
        s0.fs_type == n::MILKFS_MAGIC
            && s0.bsize == 4096
            && s0.blocks == 4096
            && s0.bfree > 0
            && s0.bfree < s0.blocks
            && s0.bavail == s0.bfree
            && s0.flags & n::ST_RDONLY == 0,
        &alloc::format!("milkfs statfs: 16 MiB in 4 KiB blocks, some used ({s0:?})"),
    );
    let f = alloc::format!("{vp}/statfs-test.bin");
    let data = alloc::vec![0x5au8; 256 << 10];
    let wrote = ops::write_file(&f, &data).is_ok() && super::milkfs::sync().is_ok();
    let s1 = ops::statfs(vp).unwrap_or_default();
    t.ok(
        wrote && s0.bfree - s1.bfree.min(s0.bfree) >= 64,
        &alloc::format!(
            "256 KiB written: free blocks {} -> {} (64 or more fewer)",
            s0.bfree,
            s1.bfree
        ),
    );
    t.ok(
        ops::statfs(&f).is_ok_and(|s| s == s1),
        "statfs of a file = statfs of its volume",
    );
    if t.must(
        super::milkfs::snapshot("statfs-snap"),
        "snapshot for statfs",
    )
    .is_some()
    {
        let sp = ops::statfs(&alloc::format!("{vp}/.snapshots/statfs-snap"));
        t.ok(
            sp.as_ref().is_ok_and(|s| {
                s.fs_type == n::MILKFS_MAGIC && s.flags & n::ST_RDONLY != 0 && s.blocks == 4096
            }),
            &alloc::format!("statfs of a snapshot: the volume's numbers, read-only ({sp:?})"),
        );
        let _ = super::milkfs::delete_snapshot("statfs-snap");
    }
    let _ = ops::unlink(&f);
    let _ = super::milkfs::sync();
    let s2 = ops::statfs(vp).unwrap_or_default();
    t.ok(
        s2.bfree >= s0.bfree.saturating_sub(4),
        &alloc::format!(
            "deleted again: free blocks back ({} -> {})",
            s1.bfree,
            s2.bfree
        ),
    );
}

pub fn run() {
    // Crash-test boots (scripts/crashtest.sh) do only that.
    if super::milkfs::crashtest::run() {
        return;
    }
    let mut t = T {
        checks: 0,
        failed: 0,
    };
    tmpfs_basics(&mut t);
    tmpfs_big_and_sparse(&mut t);
    tmpfs_rom(&mut t);
    tmpfs_namespace(&mut t);
    errno_paths(&mut t);
    paths_and_mounts(&mut t);
    initramfs(&mut t);
    devfs(&mut t);
    procfs_sysfs(&mut t);
    statfs_basics(&mut t);
    blockdev(&mut t);
    mac_share(&mut t);
    crypto(&mut t);
    milkfs(&mut t);
    notify_and_search(&mut t);
    if t.failed != 0 {
        // Loud, but never a panic: a self-test must not take the machine down.
        kprintln!(
            "*** fs selftest: FAIL: {} of {} checks failed (see FAIL lines above) ***",
            t.failed,
            t.checks
        );
    } else {
        kprintln!("fs selftest: PASS ({} checks)", t.checks);
    }
    for p in [
        "/proc/version",
        "/proc/uptime",
        "/proc/meminfo",
        "/proc/interrupts",
        "/proc/mounts",
    ] {
        show(p);
    }
    kprintln!("$ files list /System/Kernel/devices/platform");
    let names: Vec<String> = ops::readdir("/System/Kernel/devices/platform")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    kprintln!("{}", names.join("  "));
    show("/System/Kernel/fs/milkfs/info");
    show("/System/Settings/motd");
}
