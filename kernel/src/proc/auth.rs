//! Admin authorisation tickets (docs/security.md, "Authorisation tickets").
//!
//! A process asks to be authorised; the user proves they are an administrator with a
//! password (in the desktop's trusted panel, or typed in a terminal); the kernel then
//! attaches a ticket to *that process* for 5 minutes. Privileged operations (the
//! destructive /System/Kernel/fs/milkfs controls) accept euid 0 or a valid ticket ([`require`]).
//!
//! * Per process: keyed on pid + process start time, so a recycled pid gets nothing; not
//!   inherited by fork; kept across exec (same process); dropped at exit; revocable.
//! * The password is checked with `proc::login::authenticate`; the account must be in
//!   group admin (or root).
//! * Rate limit per requesting real uid: 3 wrong passwords in a row lock that uid out of
//!   authorising for 15 s, doubling with every further miss (max 10 min); a success
//!   resets it. Each try also costs the 500 ms of `authenticate`.
//! * Hooked into milkfs with `fs::milkfs::set_authorizer` ([`init`]).
//! * Audited: `auth.grant`, `auth.deny` (with the reason), `auth.revoke`, `auth.required`
//!   (a privileged write refused for want of a ticket).
//! * The desktop's trusted panel registers with [`set_prompt`]; the request then never
//!   shows the password to the requesting process.

use alloc::string::String;
use alloc::vec::Vec;

use super::errno::*;
use crate::sync::{FnSlot, SpinLock};

pub const TICKET_NS: u64 = 5 * 60 * 1_000_000_000;
const MAX_MISSES: u32 = 3;
const LOCKOUT_BASE_NS: u64 = 15_000_000_000;
const LOCKOUT_MAX_NS: u64 = 600_000_000_000;

#[derive(Clone)]
struct Ticket {
    pid: u32,
    start_ns: u64,
    /// The administrator who authorised it.
    uid: u32,
    expires_ns: u64,
}

struct Misses {
    uid: u32,
    count: u32,
    locked_until_ns: u64,
}

static TICKETS: SpinLock<Vec<Ticket>> = SpinLock::new(Vec::new());
static MISSES: SpinLock<Vec<Misses>> = SpinLock::new(Vec::new());

/// What the trusted panel returns: (account name, password), or None if cancelled.
pub type Prompt = fn(pid: u32, reason: &str, default_user: &str) -> Option<(String, String)>;
static PROMPT: FnSlot = FnSlot::new();

/// The desktop registers its trusted password panel. It runs on the requesting process's
/// thread and blocks until the user answers.
pub fn set_prompt(p: Prompt) {
    PROMPT.set(p);
}

fn start_of(pid: u32) -> Option<u64> {
    super::get(pid).map(|p| p.inner.lock().start_ns)
}

/// Nanoseconds left on `pid`'s ticket (None: none or expired).
pub fn left(pid: u32) -> Option<u64> {
    let start = start_of(pid)?;
    let now = crate::timer::now_ns();
    TICKETS
        .lock()
        .iter()
        .find(|t| t.pid == pid && t.start_ns == start && t.expires_ns > now)
        .map(|t| t.expires_ns - now)
}

/// The administrator `pid`'s ticket was granted by, if it has a valid one.
pub fn granted_by(pid: u32) -> Option<u32> {
    let start = start_of(pid)?;
    let now = crate::timer::now_ns();
    TICKETS
        .lock()
        .iter()
        .find(|t| t.pid == pid && t.start_ns == start && t.expires_ns > now)
        .map(|t| t.uid)
}

/// May the calling process do privileged things: root, or a valid ticket.
pub fn privileged() -> bool {
    if !crate::sched::started() {
        return true; // boot (the fs self-test): the kernel itself
    }
    let pid = crate::sched::current_pid();
    pid == 0 || super::current_cred().is_root() || left(pid).is_some()
}

/// [`privileged`] or EPERM (audited as `auth.required op=...`).
pub fn require(op: &str) -> KResult<()> {
    if privileged() {
        return Ok(());
    }
    crate::audit!("auth.required", "op={op}");
    Err(EPERM)
}

/// Apps the desktop started (`proc::spawn_with_caps`), as (pid, start time). Not
/// inherited by their children.
static DESKTOP_APPS: SpinLock<Vec<(u32, u64)>> = SpinLock::new(Vec::new());

/// The desktop started `pid` (Disk Utility, the Store, ...).
pub fn mark_desktop_app(pid: u32) {
    if let Some(start) = start_of(pid) {
        DESKTOP_APPS.lock().push((pid, start));
    }
}

fn is_desktop_app(pid: u32) -> bool {
    let Some(start) = start_of(pid) else {
        return false;
    };
    DESKTOP_APPS.lock().contains(&(pid, start))
}

/// The rule for milkfs's destructive controls (`fs::milkfs::DESTRUCTIVE`, through
/// `fs::milkfs::authorize`): root or a ticket, with the reason in /System/Kernel/fs/milkfs/error.
///
/// Transitional: until the window manager registers its trusted panel ([`set_prompt`]),
/// a desktop app has no way to ask for a ticket, so apps the desktop started keep the
/// old rule (the open-time admin-group check), audited as `auth.legacy`. Anything run
/// from a terminal needs a ticket now; once the panel exists, everything does.
fn milkfs_rule(op: &str) -> crate::fs::Result<()> {
    if privileged() {
        return Ok(());
    }
    let pid = crate::sched::current_pid();
    if PROMPT.get::<Prompt>().is_none() && is_desktop_app(pid) {
        crate::audit!(
            "auth.legacy",
            "pid={pid} op=milkfs.{op} (no trusted panel yet)"
        );
        return Ok(());
    }
    if require(&alloc::format!("milkfs.{op}")).is_ok() {
        return Ok(());
    }
    crate::fs::milkfs::set_reason(
        "needs an administrator's authorisation: run the command under `auth` (e.g. `auth snap delete NAME`) or approve it in the desktop's password panel",
    );
    Err(crate::fs::Errno::EPERM)
}

/// Hook the ticket rule into the filesystem. Called from `proc::init`.
pub fn init() {
    crate::fs::milkfs::set_authorizer(Some(milkfs_rule));
}

/// Drop `pid`'s ticket (exit, revoke).
pub fn forget(pid: u32) {
    TICKETS.lock().retain(|t| t.pid != pid);
    DESKTOP_APPS.lock().retain(|&(p, _)| p != pid);
}

pub fn revoke() {
    let pid = crate::sched::current_pid();
    let had = left(pid).is_some();
    forget(pid);
    if had {
        crate::audit!("auth.revoke", "pid={pid}");
    }
}

fn locked_out(ruid: u32) -> Option<u64> {
    let now = crate::timer::now_ns();
    MISSES
        .lock()
        .iter()
        .find(|m| m.uid == ruid && m.locked_until_ns > now)
        .map(|m| m.locked_until_ns - now)
}

fn missed(ruid: u32) {
    let now = crate::timer::now_ns();
    let mut m = MISSES.lock();
    let e = match m.iter_mut().position(|x| x.uid == ruid) {
        Some(i) => &mut m[i],
        None => {
            m.push(Misses {
                uid: ruid,
                count: 0,
                locked_until_ns: 0,
            });
            m.last_mut().unwrap()
        }
    };
    e.count += 1;
    if e.count >= MAX_MISSES {
        let k = (e.count - MAX_MISSES).min(10);
        e.locked_until_ns = now + (LOCKOUT_BASE_NS << k).min(LOCKOUT_MAX_NS);
    }
}

fn reset(ruid: u32) {
    MISSES.lock().retain(|m| m.uid != ruid);
}

/// Check `user`'s password (an administrator) for the calling process and attach a
/// ticket. `via`: "panel" or "password" (for the audit record). Returns the ticket's
/// lifetime in ns.
pub fn grant(user: &str, password: &str, via: &str, reason: &str) -> KResult<u64> {
    let pid = crate::sched::current_pid();
    let ruid = super::current_cred().ruid;
    let exe = super::get(pid)
        .map(|p| p.inner.lock().exe.clone())
        .unwrap_or_default();
    if let Some(left) = locked_out(ruid) {
        crate::audit!(
            "auth.deny",
            "pid={pid} exe={exe} user={user} via={via} reason=rate-limited ({} s left)",
            left / 1_000_000_000
        );
        return Err(EAGAIN);
    }
    let acct = match super::login::authenticate(user, password) {
        Ok(a) => a,
        Err(e) => {
            missed(ruid);
            crate::audit!(
                "auth.deny",
                "pid={pid} exe={exe} user={user} via={via} reason={e:?}"
            );
            return Err(EACCES);
        }
    };
    if !acct.is_admin() {
        crate::audit!(
            "auth.deny",
            "pid={pid} exe={exe} user={user} via={via} reason=not-an-administrator"
        );
        return Err(EPERM);
    }
    reset(ruid);
    let start = start_of(pid).ok_or(ESRCH)?;
    let expires = crate::timer::now_ns() + TICKET_NS;
    {
        let mut t = TICKETS.lock();
        let now = crate::timer::now_ns();
        t.retain(|x| x.pid != pid && x.expires_ns > now);
        t.push(Ticket {
            pid,
            start_ns: start,
            uid: acct.uid,
            expires_ns: expires,
        });
    }
    crate::audit!(
        "auth.grant",
        "pid={pid} exe={exe} user={} uid={} via={via} for={} s reason={reason}",
        acct.name,
        acct.uid,
        TICKET_NS / 1_000_000_000
    );
    Ok(TICKET_NS)
}

/// The desktop path: ask the trusted panel (ENOTSUP without a desktop), then [`grant`].
pub fn authorize_with_panel(reason: &str) -> KResult<u64> {
    let pid = crate::sched::current_pid();
    let Some(prompt) = PROMPT.get::<Prompt>() else {
        return Err(Errno(95)); // EOPNOTSUPP: no trusted panel (no desktop): use `auth`
    };
    let default_user = super::login::session().name;
    match prompt(pid, reason, &default_user) {
        Some((user, password)) => grant(&user, &password, "panel", reason),
        None => {
            crate::audit!("auth.deny", "pid={pid} via=panel reason=cancelled");
            Err(Errno(125)) // ECANCELED
        }
    }
}
