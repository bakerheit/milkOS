//! Per-process capability set: what a process is allowed to touch. Inherited across fork
//! and exec, and it can only ever *shrink* (a child never regains what its parent dropped).
//! The syscall layer checks it on `openat`, `execve`, socket/connect/sendto, and device
//! opens. The default set grants everything, so an ordinary process is unaffected; a
//! `sandbox` launcher installs a smaller set before `execve`. Every denial is audited.
//!
//! Grants are matched against the *normalised absolute path* a request resolves to (string
//! based: a symlink out of a granted subtree is a known gap, like an early LSM; see
//! docs/security.md).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::errno::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Net {
    None,
    /// Loopback / link-local only (127.0.0.0/8, ::1, the DHCP link): no routed traffic.
    Local,
    All,
}

/// One filesystem grant: a path prefix and whether writes are allowed.
#[derive(Clone)]
pub struct FsGrant {
    pub prefix: String,
    pub write: bool,
}

#[derive(Clone)]
pub struct Caps {
    /// `None` = unrestricted filesystem (the default). `Some(list)` = only these prefixes.
    pub fs: Option<Vec<FsGrant>>,
    pub net: Net,
    pub audio: bool,
    pub clipboard: bool,
    pub windows: bool,
    /// May fork / spawn new processes (threads are always allowed).
    pub spawn: bool,
}

impl Caps {
    /// The default: everything allowed (nothing breaks for a normal process).
    pub fn all() -> Arc<Caps> {
        Arc::new(Caps {
            fs: None,
            net: Net::All,
            audio: true,
            clipboard: true,
            windows: true,
            spawn: true,
        })
    }

    pub fn restricted(&self) -> bool {
        self.fs.is_some()
            || self.net != Net::All
            || !self.audio
            || !self.clipboard
            || !self.windows
            || !self.spawn
    }

    /// Does the fs grant cover `path` (already normalised, absolute) for the wanted access?
    pub fn fs_ok(&self, path: &str, write: bool) -> bool {
        match &self.fs {
            None => true,
            Some(grants) => grants
                .iter()
                .any(|g| (!write || g.write) && path_under(path, &g.prefix)),
        }
    }

    /// Shrink to the intersection with `other` (a child can only lose capabilities). Used
    /// to add a grant: the powerbox adds a single path to a copy, never widening anything
    /// else.
    pub fn with_fs_grant(&self, grant: FsGrant) -> Caps {
        let mut c = self.clone();
        if let Some(list) = &mut c.fs {
            list.push(grant);
        }
        // If fs was unrestricted, adding a grant doesn't restrict it; a grant only matters
        // once the set is restricted.
        c
    }
}

/// True if `path` is `prefix` itself or inside it. Both are normalised absolute paths.
impl Caps {
    /// One line in the sandbox's own spec syntax (`fs=/etc:r fs=/tmp/x:rw net=local
    /// audio=0 ...`, so it can be fed back to `sandbox`), or `all` when unrestricted. For
    /// /proc/<pid>/status.
    pub fn summary(&self) -> String {
        if !self.restricted() {
            return String::from("all");
        }
        let fs = match &self.fs {
            None => String::new(), // unrestricted: no fs token
            Some(g) if g.is_empty() => String::from("fs-none"),
            Some(g) => g
                .iter()
                .map(|x| alloc::format!("fs={}:{}", x.prefix, if x.write { "rw" } else { "r" }))
                .collect::<Vec<_>>()
                .join(" "),
        };
        let net = match self.net {
            Net::None => "none",
            Net::Local => "local",
            Net::All => "all",
        };
        let b = |v: bool| if v { 1 } else { 0 };
        alloc::format!(
            "{fs}{}net={net} audio={} clipboard={} windows={} spawn={}",
            if fs.is_empty() { "" } else { " " },
            b(self.audio),
            b(self.clipboard),
            b(self.windows),
            b(self.spawn)
        )
    }
}

pub fn path_under(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    let prefix = prefix.trim_end_matches('/');
    path == prefix
        || (path.len() > prefix.len()
            && path.starts_with(prefix)
            && path.as_bytes()[prefix.len()] == b'/')
}

/// Normalise `path` to an absolute path given the caller's cwd (`.`/`..`/`//` collapsed).
/// `..` can't climb above `/`. No symlink resolution (string only).
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let base = if path.starts_with('/') { "" } else { cwd };
    for comp in base.split('/').chain(path.split('/')) {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut s = String::from("/");
    s.push_str(&parts.join("/"));
    s
}

/// Parse a sandbox spec and return the capability set shrunk from `cur` (never widened).
/// Tokens (whitespace separated):
///   `net=none|local|all`  `audio=0|1`  `clipboard=0|1`  `windows=0|1`  `spawn=0|1`
///   `fs=<path>:r|rw`  (adds a grant; any fs= switches to a restricted list)
///   `fs-none`  (restricted, no fs access)
pub fn shrink(cur: &Caps, spec: &str) -> KResult<Caps> {
    let mut c = cur.clone();
    let order = |n: Net| match n {
        Net::None => 0,
        Net::Local => 1,
        Net::All => 2,
    };
    let mut fs_req: Option<Vec<FsGrant>> = None;
    for tok in spec.split_whitespace() {
        let (k, v) = tok.split_once('=').unwrap_or((tok, ""));
        match k {
            "net" => {
                let want = match v {
                    "none" => Net::None,
                    "local" => Net::Local,
                    "all" => Net::All,
                    _ => return Err(EINVAL),
                };
                if order(want) < order(c.net) {
                    c.net = want;
                }
            }
            "audio" => c.audio &= v != "0",
            "clipboard" => c.clipboard &= v != "0",
            "windows" => c.windows &= v != "0",
            "spawn" => c.spawn &= v != "0",
            "fs-none" => {
                fs_req.get_or_insert_with(Vec::new);
            }
            "fs" => {
                let (path, mode) = v.rsplit_once(':').unwrap_or((v, "r"));
                if path.is_empty() || !path.starts_with('/') {
                    return Err(EINVAL);
                }
                fs_req.get_or_insert_with(Vec::new).push(FsGrant {
                    prefix: normalize("/", path),
                    write: mode.contains('w'),
                });
            }
            _ => return Err(EINVAL),
        }
    }
    // Apply the requested fs grants, each clamped to what the parent already allows.
    if let Some(req) = fs_req {
        let allowed: Vec<FsGrant> = req
            .into_iter()
            .filter(|g| cur.fs_ok(&g.prefix, false))
            .map(|g| FsGrant {
                write: g.write && cur.fs_ok(&g.prefix, true),
                prefix: g.prefix,
            })
            .collect();
        c.fs = Some(allowed);
    }
    Ok(c)
}

// ---- the result of a denied check, for the syscall layer -----------------------------

/// Record a denial and return EACCES.
pub fn deny(op: &str, detail: &str) -> Errno {
    crate::audit::record("sandbox.deny", &alloc::format!("op={op} {detail}"));
    EACCES
}
