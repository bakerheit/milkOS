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
    let up = read_str("/proc/uptime");
    let secs: f32 = up
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1.0);
    t.ok(secs > 0.5 && secs < 600.0, "/proc/uptime is plausible");
    t.ok(
        read_str("/proc/version").starts_with("milkOS 0.0.1"),
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
            && mounts.contains("sysfs /sys sysfs"),
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
    let devs = ops::readdir("/sys/devices/platform").unwrap_or_default();
    t.ok(
        devs.iter().any(|e| e.name == "9000000.pl011"),
        "/sys/devices/platform lists the UART",
    );
    t.ok(
        read_str("/sys/devices/platform/9000000.pl011/compatible").contains("arm,pl011"),
        "sysfs compatible",
    );
    t.ok(
        read_str("/sys/devices/platform/9000000.pl011/reg").starts_with("0x9000000 0x1000"),
        "sysfs reg",
    );
    t.ok(
        read_str("/sys/kernel/version") == read_str("/proc/version"),
        "/sys/kernel/version",
    );
    t.ok(
        ops::lookup("/sys/devices/platform/..")
            .and_then(|i| path::path_of(&i))
            .ok()
            .as_deref()
            == Some("/sys/devices"),
        "sysfs .. and path_of",
    );
}

/// Disk I/O through the first block device, if there is one. Non-destructive: the region
/// written (the last 80 KiB, bigger than the driver's 64 KiB bounce buffer) is restored.
fn blockdev(t: &mut T) {
    // The scratch disk: the first one that does not hold milkfs.
    let Some(d) = super::blockdev::devices()
        .into_iter()
        .find(|d| super::milkfs::probe(d).is_none())
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
    let info = read_str("/sys/fs/milkfs/info");
    t.ok(
        info.contains("state:       ok")
            && info.contains(&alloc::format!("generation:  {generation}")),
        "user volume info consistent",
    );
    milk_space(t, &v, "user volume space accounting exact");
    // Persistent home: /home/milk is a bind mount of <volume>/home/milk.
    let vol_dev = ops::stat(&vp).map(|s| s.dev).ok();
    t.ok(
        ops::stat("/home/milk").map(|s| s.dev).ok() == vol_dev,
        "/home/milk is on milkfs",
    );
    let pathof = |p: &str| {
        ops::lookup(p)
            .and_then(|i| path::path_of(&i))
            .unwrap_or_default()
    };
    t.ok(
        pathof("/home/milk") == "/home/milk",
        "path_of home is /home/milk",
    );
    t.ok(
        pathof("/Users/milk") == "/home/milk",
        "/Users/milk is the same home",
    );
    t.err(
        ops::rmdir(&alloc::format!("{vp}/home/milk")),
        Errno::EBUSY,
        "the bound home can't be removed",
    );
}

/// Everything destructive, on a throwaway milkfs in RAM (mkfs'd here, unmounted after).
fn milkfs_test_volume(t: &mut T) {
    use super::blockdev::BlockRef;
    const VP: &str = "/Volumes/.milk-selftest";
    const D: &str = "/Volumes/.milk-selftest/t";
    let free0 = pmm::stats().free_frames;
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
    super::milkfs::set_test_override(Some(vol.clone()));
    milkfs_destructive(t, &vol, VP, D, &dev);
    milkfs_clones(t, &vol, VP, &dev);
    milkfs_bench(&vol, VP);
    super::milkfs::set_test_override(None);
    t.ok(super::unmount(VP).is_ok(), "unmount the test volume");
    let _ = ops::rmdir(VP);
    drop(vol);
    drop(dev);
    t.ok(
        pmm::stats().free_frames == free0,
        "test volume's RAM disk freed",
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
        ops::write_file("/sys/fs/milkfs/snapshot", b"selftest-b\n").is_ok(),
        "snapshot via /sys",
    );
    let list = read_str("/sys/fs/milkfs/snapshots");
    t.ok(
        list.contains("selftest-a gen=") && list.contains("selftest-b gen="),
        "/sys/fs/milkfs/snapshots",
    );
    t.ok(
        read_str("/sys/fs/milkfs/info").contains("label:       MilkTest"),
        "/sys/fs/milkfs/info",
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
        ops::write_file("/sys/fs/milkfs/rollback", b"selftest-a").is_ok(),
        "rollback via /sys",
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
            super::ops::write_file("/sys/fs/milkfs/auto", b"interval=30s keep=3 daily=2").is_ok()
                && v.state.lock().sb.auto == [1, 30, 3, 2]
                && read_str("/sys/fs/milkfs/auto").starts_with("on interval=30s keep=3 daily=2"),
            "/sys/fs/milkfs/auto sets and persists the settings",
        );
        t.err(
            super::ops::write_file("/sys/fs/milkfs/auto", b"interval=x"),
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
        read_str("/sys/fs/milkfs/clones").contains("try of=base at=/Volumes/MilkTest-try"),
        "/sys/fs/milkfs/clones",
    );
    milk_space(t, vol, "milkfs space exact with a clone");

    // Discard: a second clone with a big file, thrown away.
    t.ok(
        ops::write_file("/sys/fs/milkfs/clone", b"base scratch").is_ok(),
        "clone via /sys",
    );
    let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    t.ok(
        ops::write_file("/Volumes/MilkTest-scratch/big", &big).is_ok(),
        "write in the second clone",
    );
    milk_space(t, vol, "milkfs space exact with two clones");
    let free_with = vol.state.lock().store.free_blocks();
    t.ok(
        ops::write_file("/sys/fs/milkfs/discard", b"scratch").is_ok(),
        "discard via /sys",
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

/// Small-file benchmark on the test volume: printed, not checked.
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
    kprintln!("$ cat {path}");
    kprintln!("{}", read_str(path).trim_end());
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
    tmpfs_namespace(&mut t);
    errno_paths(&mut t);
    paths_and_mounts(&mut t);
    initramfs(&mut t);
    devfs(&mut t);
    procfs_sysfs(&mut t);
    blockdev(&mut t);
    milkfs(&mut t);
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
    kprintln!("$ ls /sys/devices/platform");
    let names: Vec<String> = ops::readdir("/sys/devices/platform")
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    kprintln!("{}", names.join("  "));
    show("/sys/fs/milkfs/info");
    show("/etc/motd");
}
