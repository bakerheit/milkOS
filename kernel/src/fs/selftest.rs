//! Boot self-test for the VFS, tmpfs, initramfs, devfs and procfs/sysfs. Panics on failure
//! (like `memtest`), then prints `/etc/motd`.

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
        ops::open("/etc/motd", O_RDONLY | O_CREAT | O_EXCL, 0),
        Errno::EEXIST,
        "O_EXCL on existing",
    );
    t.err(
        ops::open("/etc/motd/x", O_RDONLY, 0),
        Errno::ENOTDIR,
        "file used as dir",
    );
    t.err(
        ops::open("/etc/motd/", O_RDONLY, 0),
        Errno::ENOTDIR,
        "trailing slash on a file",
    );
    t.err(
        ops::open("/etc", O_WRONLY, 0),
        Errno::EISDIR,
        "open dir for writing",
    );
    t.err(
        ops::open("/etc/motd", O_RDONLY | O_DIRECTORY, 0),
        Errno::ENOTDIR,
        "O_DIRECTORY on a file",
    );
    t.err(ops::open("", O_RDONLY, 0), Errno::ENOENT, "empty path");
    t.err(
        ops::open("/etc/motd", O_ACCMODE, 0),
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
    if let Some(f) = t.must(ops::open("/etc/motd", O_RDONLY, 0), "open motd") {
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
        ops::rename("/etc/hostname", "/dev/hostname"),
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
        read_str("/home/milk/../../etc/hostname") == "milk\n",
        "../.. walk",
    );
    t.ok(read_str("//etc///hostname") == "milk\n", "repeated slashes");
    t.ok(
        read_str("/Users/milk/README.txt").contains("home directory"),
        "/Users -> home symlink",
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
    t.ok(pathof("/home/milk") == "/home/milk", "path_of /home/milk");
    t.ok(
        pathof("/Users/milk") == "/home/milk",
        "path_of through a symlink",
    );
    t.ok(
        pathof("/dev") == "/dev" && pathof("/") == "/",
        "path_of across a mount",
    );
    if let Some(home) = t.must(ops::lookup("/home"), "lookup /home") {
        t.ok(
            ops::stat_at(Some(&home), "milk/README.txt", true).is_ok(),
            "relative path from a base dir",
        );
        t.ok(
            ops::stat_at(Some(&home), "/etc/motd", true).is_ok(),
            "absolute path ignores the base",
        );
    }
}

fn initramfs(t: &mut T) {
    t.ok(read_str("/etc/hostname") == "milk\n", "/etc/hostname");
    t.ok(
        read_str("/etc/motd").contains("Welcome to milkOS"),
        "/etc/motd",
    );
    t.ok(
        read_str("/System/Version.txt").contains("ProductName:    milkOS"),
        "/System/Version.txt",
    );
    t.ok(
        read_str("/home/milk/README.txt").starts_with("Hi"),
        "/home/milk/README.txt",
    );
    t.ok(
        ops::stat("/tmp").map(|s| s.perm).ok() == Some(0o1777),
        "/tmp is sticky + world-writable",
    );
    for d in [
        "/Applications",
        "/System",
        "/bin",
        "/home/milk",
        "/Volumes",
        "/proc",
        "/sys",
    ] {
        t.ok(
            ops::stat(d).map(|s| s.kind).ok() == Some(InodeType::Dir),
            "skeleton dir exists",
        );
    }
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

pub fn run() {
    let mut t = T {
        checks: 0,
        failed: 0,
    };
    tmpfs_basics(&mut t);
    tmpfs_big_and_sparse(&mut t);
    tmpfs_namespace(&mut t);
    errno_paths(&mut t);
    paths_and_mounts(&mut t);
    initramfs(&mut t);
    devfs(&mut t);
    if t.failed != 0 {
        panic!("fs selftest: {} of {} checks failed", t.failed, t.checks);
    }
    kprintln!("fs selftest: PASS ({} checks)", t.checks);
    kprintln!("$ cat /etc/motd");
    kprintln!("{}", read_str("/etc/motd").trim_end());
}
