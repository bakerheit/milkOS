//! milkd: the service manager (docs/processes.md, "Services").
//!
//! Started by init (PID 1) as root. Reads /etc/milk/services/*.service, starts the enabled
//! units in dependency order as their declared users with their sandbox, restarts them
//! with backoff, forwards their stdout / stderr to the journal (/dev/journal), and serves
//! `svc` on /run/milkd.sock (AF_UNIX, SO_PEERCRED: anyone may list / status, start / stop /
//! restart / reload need root or group admin). One thread, an epoll loop.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use rt::epoll::{self as ep, EPOLLIN};
use rt::unix::{self, Name};
use rt::{sys, users};

rt::main!(main);

const DIR: &str = "/etc/milk/services";
const SOCK: &str = "/run/milkd.sock";
const ADMIN_GID: u32 = 80;
const BACKOFF_START_MS: u64 = 100;
const BACKOFF_MAX_MS: u64 = 30_000;
const QUICK_MS: u64 = 10_000;
const MAX_QUICK_FAILURES: u32 = 8;
const STOP_GRACE_MS: u64 = 3_000;

// epoll data tags: kind << 32 | index
const TAG_LISTENER: u64 = 1 << 32;
const TAG_CLIENT: u64 = 2 << 32;
const TAG_OUT: u64 = 3 << 32;
const TAG_ERR: u64 = 4 << 32;
const TAG_ACTIVATE: u64 = 5 << 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Restart {
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Debug, Default)]
struct Def {
    description: String,
    exec: Vec<String>,
    user: String,
    restart: Option<Restart>,
    after: Vec<String>,
    needs: Vec<String>,
    sandbox: Option<String>,
    socket: Option<String>,
    cwd: String,
    env: Vec<String>,
    if_exists: Option<String>,
    if_contains: Option<(String, String)>,
    enabled: bool,
    /// Also echo the unit's output (and its start line) on milkd's console.
    console: bool,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for its dependencies to come up.
    Pending,
    Running,
    /// Being stopped: SIGTERM sent, SIGKILL at the deadline.
    Stopping,
    /// Will be started again at the deadline (backoff).
    Waiting,
    /// Socket-activated: listening, starts on the first connection.
    Listening,
    /// Exited and not restarted (exit status).
    Exited(i32),
    /// Stopped by `svc stop`, or never enabled.
    Inactive,
    Skipped(String),
    Failed(String),
}

struct Unit {
    /// Stable id for epoll tags (units only ever get added).
    id: u64,
    name: String,
    def: Def,
    state: State,
    pid: u32,
    started_ms: u64,
    /// Restart or SIGKILL time (Waiting, Stopping).
    deadline_ms: u64,
    restarts: u32,
    quick_failures: u32,
    last_status: Option<i32>,
    out_fd: i32,
    err_fd: i32,
    out_buf: Vec<u8>,
    err_buf: Vec<u8>,
    listen_fd: i32,
    /// Start again once it's down (svc restart).
    restart_after_stop: bool,
    /// Being stopped because a unit it needs went down: it ends up Failed with this.
    dep_failed: Option<String>,
}

fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

/// Split an `exec` line on spaces; `"..."` quotes one argument.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(core::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn parse(text: &str) -> Def {
    let mut d = Def {
        user: String::from("root"),
        cwd: String::from("/"),
        enabled: true,
        ..Default::default()
    };
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            d.warnings.push(alloc::format!("line {}: no '='", n + 1));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "exec" => d.exec = split_args(v),
            "description" => d.description = String::from(v),
            "user" => d.user = String::from(v),
            "restart" => {
                d.restart = match v {
                    "always" => Some(Restart::Always),
                    "on-failure" => Some(Restart::OnFailure),
                    "never" => Some(Restart::Never),
                    _ => {
                        d.warnings.push(alloc::format!("restart = {v}?"));
                        None
                    }
                }
            }
            "after" => d.after = words(v),
            "needs" => d.needs = words(v),
            "sandbox" => d.sandbox = Some(String::from(v)),
            "socket" => d.socket = Some(String::from(v)),
            "cwd" => d.cwd = String::from(v),
            "env" => d.env = words(v),
            "if-exists" => d.if_exists = Some(String::from(v)),
            "if-contains" => {
                if let Some((p, t)) = v.split_once(' ') {
                    d.if_contains = Some((String::from(p), String::from(t.trim_start())));
                }
            }
            "enabled" => d.enabled = matches!(v, "yes" | "true" | "1"),
            "console" => d.console = matches!(v, "yes" | "true" | "1"),
            other => d.warnings.push(alloc::format!("unknown key {other}")),
        }
    }
    if d.exec.is_empty() || !d.exec[0].starts_with('/') {
        d.warnings
            .push(String::from("exec missing or not an absolute path"));
    }
    d
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

struct Milkd {
    units: BTreeMap<String, Unit>,
    ep: i32,
    journal: i32,
    listener: i32,
    clients: BTreeMap<i32, Vec<u8>>,
    next_id: u64,
}

impl Milkd {
    fn log(&self, unit: &str, level: &str, pid: u32, msg: &str) {
        if self.journal >= 0 {
            let line = alloc::format!("{unit}\t{level}\t{pid}\t{msg}\n");
            let _ = sys::write_all(self.journal, line.as_bytes());
        }
    }

    fn note(&self, msg: &str) {
        self.log("milkd", "info", sys::getpid(), msg);
    }

    fn load(&mut self) {
        let mut found = Vec::new();
        for (name, _) in sys::read_dir(DIR).unwrap_or_default() {
            let Some(unit) = name.strip_suffix(".service") else {
                continue;
            };
            if !valid_name(unit) {
                self.note(&alloc::format!("ignoring {name}: bad unit name"));
                continue;
            }
            let Ok(text) = sys::read_file(&alloc::format!("{DIR}/{name}")) else {
                continue;
            };
            let def = parse(&String::from_utf8_lossy(&text));
            for w in &def.warnings {
                self.log(
                    "milkd",
                    "warn",
                    sys::getpid(),
                    &alloc::format!("{unit}: {w}"),
                );
            }
            found.push(String::from(unit));
            match self.units.get_mut(unit) {
                Some(u) => u.def = def, // reload: takes effect on the next start
                None => {
                    let state = if def.enabled {
                        State::Pending
                    } else {
                        State::Inactive
                    };
                    self.next_id += 1;
                    self.units.insert(
                        String::from(unit),
                        Unit {
                            id: self.next_id,
                            name: String::from(unit),
                            def,
                            state,
                            pid: 0,
                            started_ms: 0,
                            deadline_ms: 0,
                            restarts: 0,
                            quick_failures: 0,
                            last_status: None,
                            out_fd: -1,
                            err_fd: -1,
                            out_buf: Vec::new(),
                            err_buf: Vec::new(),
                            listen_fd: -1,
                            restart_after_stop: false,
                            dep_failed: None,
                        },
                    );
                }
            }
        }
        self.note(&alloc::format!(
            "{} units: {}",
            found.len(),
            found.join(" ")
        ));
    }

    /// Start whatever is Pending and whose dependencies allow it; mark dependency failures.
    fn schedule(&mut self) {
        loop {
            let mut changed = false;
            let names: Vec<String> = self.units.keys().cloned().collect();
            for n in names {
                if self.units[&n].state != State::Pending {
                    continue;
                }
                let def = self.units[&n].def.clone();
                // needs: must be up; a dead end fails this unit too.
                let mut verdict = None;
                for d in &def.needs {
                    match self.units.get(d).map(|u| u.state.clone()) {
                        None => {
                            verdict =
                                Some(Err(alloc::format!("dependency failed: {d} (no such unit)")))
                        }
                        Some(State::Running | State::Listening | State::Exited(0)) => {}
                        Some(
                            State::Failed(_)
                            | State::Skipped(_)
                            | State::Inactive
                            | State::Exited(_),
                        ) => verdict = Some(Err(alloc::format!("dependency failed: {d}"))),
                        Some(_) => {
                            if verdict.is_none() {
                                verdict = Some(Ok(())); // still coming up: wait
                            }
                        }
                    }
                    if matches!(verdict, Some(Err(_))) {
                        break;
                    }
                }
                if let Some(Err(why)) = verdict {
                    self.log(&n, "err", 0, &why);
                    self.units.get_mut(&n).unwrap().state = State::Failed(why);
                    changed = true;
                    continue;
                }
                if verdict.is_some() {
                    continue; // waiting on a need
                }
                // after: ordering only; wait while one of them is still Pending.
                if def
                    .after
                    .iter()
                    .any(|d| self.units.get(d).is_some_and(|u| u.state == State::Pending))
                {
                    continue;
                }
                self.start(&n);
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    fn start(&mut self, n: &str) {
        let def = self.units[n].def.clone();
        if let Some(p) = &def.if_exists
            && sys::stat_path(p, true).is_err()
        {
            self.units.get_mut(n).unwrap().state =
                State::Skipped(alloc::format!("{p} does not exist"));
            return;
        }
        if let Some((p, t)) = &def.if_contains
            && !sys::read_file(p)
                .is_ok_and(|d| d.windows(t.len().max(1)).any(|w| w == t.as_bytes()))
        {
            self.units.get_mut(n).unwrap().state =
                State::Skipped(alloc::format!("{p} does not contain {t:?}"));
            return;
        }
        if def.exec.is_empty() || !def.exec[0].starts_with('/') {
            self.units.get_mut(n).unwrap().state = State::Failed(String::from("no exec"));
            return;
        }
        let Some(acct) = users::by_name(&def.user) else {
            let why = alloc::format!("unknown user {}", def.user);
            self.log(n, "err", 0, &why);
            self.units.get_mut(n).unwrap().state = State::Failed(why);
            return;
        };
        // Socket activation: listen now, start on the first connection.
        if let Some(path) = &def.socket {
            let u = self.units.get_mut(n).unwrap();
            if u.listen_fd < 0 {
                let _ = sys::unlink(path);
                let Ok(fd) = unix::socket(unix::SOCK_STREAM | unix::SOCK_CLOEXEC) else {
                    u.state = State::Failed(String::from("socket"));
                    return;
                };
                if let Err(e) = unix::bind(fd, &Name::Path(path)) {
                    u.state = State::Failed(alloc::format!("bind {path}: {e}"));
                    let _ = sys::close(fd);
                    return;
                }
                let _ = unix::listen(fd, 64);
                let _ = sys::chmod(path, 0o660);
                let _ = sys::chown(path, acct.uid, ADMIN_GID);
                u.listen_fd = fd;
            }
            if u.state != State::Listening && u.pid == 0 && !u.restart_after_stop {
                let idx = self.index_of(n);
                let fd = self.units[n].listen_fd;
                let _ = ep::add(self.ep, fd, EPOLLIN, TAG_ACTIVATE | idx);
                self.units.get_mut(n).unwrap().state = State::Listening;
                self.note(&alloc::format!("{n}: listening on {path}"));
                return;
            }
        }
        self.spawn(n, &acct);
    }

    fn index_of(&self, n: &str) -> u64 {
        self.units[n].id
    }

    fn by_index(&self, i: u64) -> Option<String> {
        self.units
            .values()
            .find(|u| u.id == i)
            .map(|u| u.name.clone())
    }

    fn spawn(&mut self, n: &str, acct: &users::Passwd) {
        let def = self.units[n].def.clone();
        let (Ok((or, ow)), Ok((er, ew))) = (sys::pipe2(sys::O_CLOEXEC), sys::pipe2(sys::O_CLOEXEC))
        else {
            self.units.get_mut(n).unwrap().state = State::Failed(String::from("pipe"));
            return;
        };
        let listen_fd = self.units[n].listen_fd;
        let mut env: Vec<String> = users::login_env(acct);
        env.extend(def.env.iter().cloned());
        let pid = match sys::fork() {
            Ok(0) => {
                // The child: its own process group, output to milkd, ids, sandbox, exec.
                let _ = sys::setpgid(0, 0);
                if let Ok(null) = sys::open("/dev/null", sys::O_RDONLY) {
                    let _ = sys::dup2(null, 0);
                }
                let _ = sys::dup2(ow, 1);
                let _ = sys::dup2(ew, 2);
                let mut env = env;
                if listen_fd >= 0 {
                    let _ = sys::dup2(listen_fd, 3);
                    env.push(String::from("LISTEN_FDS=1"));
                    env.push(alloc::format!("LISTEN_PID={}", sys::getpid()));
                }
                if let Err(e) = users::become_user(acct) {
                    rt::eprintln!("milkd: cannot become {}: {e}", acct.name);
                    sys::exit(126);
                }
                let _ = sys::chdir(&def.cwd);
                if let Some(spec) = &def.sandbox {
                    // The service's own binary is always readable.
                    let spec = with_binary_grant(spec, &def.exec[0]);
                    if let Err(e) = sys::sandbox(&spec) {
                        rt::eprintln!("milkd: sandbox {spec:?}: {e}");
                        sys::exit(126);
                    }
                }
                let argv: Vec<&str> = def.exec.iter().map(String::as_str).collect();
                let envp: Vec<&str> = env.iter().map(String::as_str).collect();
                let e = sys::execve(argv[0], &argv, &envp);
                rt::eprintln!("milkd: {}: {e}", argv[0]);
                sys::exit(127);
            }
            Ok(pid) => pid,
            Err(e) => {
                let _ = sys::close(or);
                let _ = sys::close(ow);
                let _ = sys::close(er);
                let _ = sys::close(ew);
                self.units.get_mut(n).unwrap().state = State::Failed(alloc::format!("fork: {e}"));
                return;
            }
        };
        let _ = sys::close(ow);
        let _ = sys::close(ew);
        let idx = self.index_of(n);
        let _ = ep::add(self.ep, or, EPOLLIN, TAG_OUT | idx);
        let _ = ep::add(self.ep, er, EPOLLIN, TAG_ERR | idx);
        let u = self.units.get_mut(n).unwrap();
        u.pid = pid;
        u.out_fd = or;
        u.err_fd = er;
        u.state = State::Running;
        u.started_ms = now_ms();
        let user = acct.name.clone();
        self.log(
            n,
            "info",
            pid,
            &alloc::format!("started as {user} (pid {pid})"),
        );
        // The console line init used to print for its daemons.
        let d = &self.units[n].def.description;
        if d.is_empty() {
            rt::println!("  {n} (pid {pid})");
        } else {
            rt::println!("  {n} (pid {pid}): {d}");
        }
    }

    /// A child exited: restart, give up, or settle.
    fn exited(&mut self, pid: u32, status: i32) {
        let Some(n) = self
            .units
            .values()
            .find(|u| u.pid == pid)
            .map(|u| u.name.clone())
        else {
            return;
        };
        self.drain_output(&n, true);
        let now = now_ms();
        let u = self.units.get_mut(&n).unwrap();
        u.pid = 0;
        u.last_status = Some(status);
        let ran = now.saturating_sub(u.started_ms);
        let failed = status != 0;
        let desc = sys::describe_status(status);
        if u.state == State::Stopping {
            let again = core::mem::take(&mut u.restart_after_stop);
            u.state = match u.dep_failed.take() {
                Some(why) => State::Failed(why),
                None => State::Inactive,
            };
            self.log(&n, "info", pid, &alloc::format!("stopped ({desc})"));
            if again {
                self.units.get_mut(&n).unwrap().state = State::Pending;
            } else if self.units[&n].def.socket.is_some() {
                // A stopped socket unit stops listening too.
                let u = self.units.get_mut(&n).unwrap();
                let _ = sys::close(u.listen_fd);
                u.listen_fd = -1;
            }
            return;
        }
        let policy = u.def.restart.unwrap_or(Restart::OnFailure);
        let restart = match policy {
            Restart::Always => true,
            Restart::OnFailure => failed,
            Restart::Never => false,
        };
        if !restart {
            u.state = State::Exited(status);
            let level = if failed { "err" } else { "info" };
            self.log(&n, level, pid, &alloc::format!("exited ({desc})"));
            return;
        }
        if ran < QUICK_MS {
            u.quick_failures += 1;
        } else {
            u.quick_failures = 1;
        }
        if u.quick_failures >= MAX_QUICK_FAILURES {
            let why = alloc::format!(
                "{} quick failures in a row, giving up ({desc})",
                u.quick_failures
            );
            u.state = State::Failed(why.clone());
            self.log(&n, "err", pid, &why);
            return;
        }
        let delay = (BACKOFF_START_MS << (u.quick_failures - 1).min(20)).min(BACKOFF_MAX_MS);
        u.deadline_ms = now + delay;
        u.state = State::Waiting;
        u.restarts += 1;
        let k = u.restarts;
        self.log(
            &n,
            "warn",
            pid,
            &alloc::format!("exited ({desc}) after {ran} ms; restart #{k} in {delay} ms"),
        );
    }

    /// Forward complete lines (all of it at EOF) from a unit's stdout / stderr.
    fn pump(&mut self, n: &str, err: bool) {
        let fd = if err {
            self.units[n].err_fd
        } else {
            self.units[n].out_fd
        };
        if fd < 0 {
            return;
        }
        let mut b = [0u8; 2048];
        let eof = match sys::read(fd, &mut b) {
            Ok(0) | Err(_) => true,
            Ok(k) => {
                let u = self.units.get_mut(n).unwrap();
                let buf = if err { &mut u.err_buf } else { &mut u.out_buf };
                buf.extend_from_slice(&b[..k]);
                false
            }
        };
        self.flush_lines(n, err, eof);
        if eof {
            let _ = ep::del(self.ep, fd);
            let _ = sys::close(fd);
            let u = self.units.get_mut(n).unwrap();
            if err {
                u.err_fd = -1;
            } else {
                u.out_fd = -1;
            }
        }
    }

    fn flush_lines(&mut self, n: &str, err: bool, all: bool) {
        let pid = self.units[n].pid;
        let mut lines = Vec::new();
        {
            let u = self.units.get_mut(n).unwrap();
            let buf = if err { &mut u.err_buf } else { &mut u.out_buf };
            while let Some(i) = buf.iter().position(|&c| c == b'\n') {
                let l: Vec<u8> = buf.drain(..=i).collect();
                lines.push(String::from_utf8_lossy(&l[..l.len() - 1]).into_owned());
            }
            if all && !buf.is_empty() {
                lines.push(String::from_utf8_lossy(buf).into_owned());
                buf.clear();
            }
        }
        let console = self.units[n].def.console;
        for l in lines {
            self.log(n, if err { "err" } else { "info" }, pid, &l);
            if console {
                rt::println!("{l}");
            }
        }
    }

    fn drain_output(&mut self, n: &str, _final: bool) {
        for err in [false, true] {
            for _ in 0..64 {
                let fd = if err {
                    self.units[n].err_fd
                } else {
                    self.units[n].out_fd
                };
                if fd < 0 {
                    break;
                }
                // Non-blocking peek via epoll: only read while it's readable.
                let ready = {
                    let mut rs = [0u64; 4];
                    rs[(fd / 64) as usize] |= 1 << (fd % 64);
                    ep::select(fd as usize + 1, Some(&mut rs), None, None, Some(0)).unwrap_or(0) > 0
                };
                if !ready {
                    break;
                }
                self.pump(n, err);
            }
        }
    }

    fn stop(&mut self, n: &str) -> Result<(), String> {
        let u = self
            .units
            .get_mut(n)
            .ok_or_else(|| String::from("no such unit"))?;
        match u.state {
            State::Running | State::Stopping => {
                if u.state == State::Running {
                    let _ = sys::kill(-(u.pid as i32), sys::SIGTERM);
                    u.deadline_ms = now_ms() + STOP_GRACE_MS;
                    u.state = State::Stopping;
                }
                Ok(())
            }
            State::Listening => {
                let _ = ep::del(self.ep, u.listen_fd);
                let _ = sys::close(u.listen_fd);
                u.listen_fd = -1;
                u.state = State::Inactive;
                Ok(())
            }
            _ => {
                u.state = State::Inactive;
                Ok(())
            }
        }
    }

    fn start_cmd(&mut self, n: &str) -> Result<(), String> {
        let u = self
            .units
            .get_mut(n)
            .ok_or_else(|| String::from("no such unit"))?;
        match u.state {
            State::Running | State::Listening | State::Pending => Ok(()),
            State::Stopping => {
                u.restart_after_stop = true;
                Ok(())
            }
            _ => {
                u.quick_failures = 0;
                u.state = State::Pending;
                Ok(())
            }
        }
    }

    fn tick(&mut self) {
        let now = now_ms();
        let names: Vec<String> = self.units.keys().cloned().collect();
        for n in names {
            let (state, deadline, pid) = {
                let u = &self.units[&n];
                (u.state.clone(), u.deadline_ms, u.pid)
            };
            if deadline > now {
                continue;
            }
            match state {
                State::Waiting => self.units.get_mut(&n).unwrap().state = State::Pending,
                State::Stopping if pid != 0 => {
                    let _ = sys::kill(-(pid as i32), sys::SIGKILL);
                    self.units.get_mut(&n).unwrap().deadline_ms = now + 1000;
                }
                _ => {}
            }
        }
        // Reap.
        while let Ok((pid, st)) = sys::wait4(-1, sys::WNOHANG) {
            if pid == 0 {
                break;
            }
            self.exited(pid, st);
        }
        self.propagate_failures();
        self.schedule();
    }

    /// A unit that `needs` one that went down (failed, skipped, stopped, exited with an
    /// error) goes down too, as "dependency failed: X".
    fn propagate_failures(&mut self) {
        let down: Vec<String> = self
            .units
            .values()
            .filter(|u| {
                matches!(
                    u.state,
                    State::Failed(_) | State::Skipped(_) | State::Inactive
                ) || matches!(u.state, State::Exited(s) if s != 0)
            })
            .map(|u| u.name.clone())
            .collect();
        let names: Vec<String> = self.units.keys().cloned().collect();
        for n in names {
            let Some(dep) = self.units[&n]
                .def
                .needs
                .iter()
                .find(|d| down.contains(d))
                .cloned()
            else {
                continue;
            };
            let why = alloc::format!("dependency failed: {dep}");
            match self.units[&n].state {
                State::Running => {
                    self.units.get_mut(&n).unwrap().dep_failed = Some(why.clone());
                    let _ = self.stop(&n);
                    self.log(&n, "err", 0, &why);
                }
                State::Waiting | State::Listening => {
                    let _ = self.stop(&n);
                    self.units.get_mut(&n).unwrap().state = State::Failed(why.clone());
                    self.log(&n, "err", 0, &why);
                }
                _ => {}
            }
        }
    }

    fn next_timeout(&self) -> i32 {
        let now = now_ms();
        let soonest = self
            .units
            .values()
            .filter(|u| matches!(u.state, State::Waiting | State::Stopping))
            .map(|u| u.deadline_ms.saturating_sub(now))
            .min()
            .unwrap_or(250);
        soonest.clamp(10, 250) as i32
    }

    // ---- the control socket ----------------------------------------------------------

    fn status_line(u: &Unit) -> String {
        let (state, detail) = match &u.state {
            State::Pending => ("pending", String::new()),
            State::Running => ("running", String::new()),
            State::Stopping => ("stopping", String::new()),
            State::Waiting => (
                "restarting",
                alloc::format!("in {} ms", u.deadline_ms.saturating_sub(now_ms())),
            ),
            State::Listening => ("listening", u.def.socket.clone().unwrap_or_default()),
            State::Exited(s) => ("exited", sys::describe_status(*s)),
            State::Inactive => ("inactive", String::new()),
            State::Skipped(w) => ("skipped", w.clone()),
            State::Failed(w) => ("failed", w.clone()),
        };
        let uptime = if u.state == State::Running {
            (now_ms() - u.started_ms) / 1000
        } else {
            0
        };
        // unit state pid restarts uptime_s user detail description warnings
        alloc::format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            u.name,
            state,
            u.pid,
            u.restarts,
            uptime,
            u.def.user,
            detail,
            u.def.description,
            u.def.warnings.join("; ")
        )
    }

    fn request(&mut self, line: &str, peer: unix::Cred) -> String {
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let arg = it.next();
        let admin = peer.uid == 0
            || users::by_uid(peer.uid)
                .is_some_and(|p| users::groups_of(&p.name, p.gid).contains(&ADMIN_GID));
        let need_admin = matches!(cmd, "start" | "stop" | "restart" | "reload");
        if need_admin && !admin {
            self.log(
                "milkd",
                "warn",
                peer.pid,
                &alloc::format!(
                    "refused {line:?} from uid {} (not an administrator)",
                    peer.uid
                ),
            );
            return String::from("error\tpermission denied: start/stop need root or group admin\n");
        }
        let one = |s: &Self, n: Option<&str>| -> Result<String, String> {
            let n = n.ok_or_else(|| String::from("which unit?"))?;
            s.units
                .get(n)
                .map(|u| alloc::format!("{}\n", Self::status_line(u)))
                .ok_or_else(|| alloc::format!("no such unit {n}"))
        };
        let r: Result<String, String> = match cmd {
            "list" | "status" if arg.is_none() => Ok(self
                .units
                .values()
                .map(|u| alloc::format!("{}\n", Self::status_line(u)))
                .collect()),
            "status" => one(self, arg),
            "start" | "stop" | "restart" => {
                let n = String::from(arg.unwrap_or(""));
                let res = match cmd {
                    "start" => self.start_cmd(&n),
                    "stop" => self.stop(&n),
                    _ => {
                        let r = self.stop(&n);
                        if r.is_ok() {
                            let u = self.units.get_mut(&n).unwrap();
                            if u.state == State::Stopping {
                                u.restart_after_stop = true;
                            } else {
                                u.state = State::Pending;
                            }
                        }
                        r
                    }
                };
                if res.is_ok() {
                    self.log(
                        &n,
                        "info",
                        peer.pid,
                        &alloc::format!("{cmd} requested by uid {}", peer.uid),
                    );
                    self.schedule();
                }
                res.and_then(|_| one(self, Some(&n)))
            }
            "reload" => {
                self.load();
                self.schedule();
                Ok(String::new())
            }
            _ => Err(alloc::format!("unknown request {cmd:?}")),
        };
        match r {
            Ok(body) => alloc::format!("{body}ok\n"),
            Err(e) => alloc::format!("error\t{e}\n"),
        }
    }

    fn client_readable(&mut self, fd: i32) {
        let mut b = [0u8; 512];
        let k = sys::read(fd, &mut b).unwrap_or(0);
        if k == 0 {
            self.close_client(fd);
            return;
        }
        let buf = self.clients.entry(fd).or_default();
        buf.extend_from_slice(&b[..k]);
        while let Some(i) = self.clients[&fd].iter().position(|&c| c == b'\n') {
            let line: Vec<u8> = self.clients.get_mut(&fd).unwrap().drain(..=i).collect();
            let line = String::from(String::from_utf8_lossy(&line).trim());
            let peer = unix::peer_cred(fd).unwrap_or(unix::Cred {
                pid: 0,
                uid: u32::MAX,
                gid: u32::MAX,
            });
            let reply = self.request(&line, peer);
            let _ = sys::write_all(fd, reply.as_bytes());
        }
    }

    fn close_client(&mut self, fd: i32) {
        let _ = ep::del(self.ep, fd);
        let _ = sys::close(fd);
        self.clients.remove(&fd);
    }
}

/// Add `fs=<binary>:r` to the spec if it restricts the file system at all (one grant per
/// `fs=` token, as the kernel's sandbox spec has it).
fn with_binary_grant(spec: &str, bin: &str) -> String {
    let restricts = spec
        .split_whitespace()
        .any(|w| w.starts_with("fs=") || w == "fs-none");
    if restricts {
        alloc::format!("{spec} fs={bin}:r")
    } else {
        String::from(spec)
    }
}

fn main(_args: &rt::Args) -> i32 {
    if sys::geteuid() != 0 {
        rt::eprintln!("milkd: must run as root (init starts it)");
        return 1;
    }
    let journal = sys::open("/dev/journal", sys::O_WRONLY | sys::O_CLOEXEC).unwrap_or(-1);
    let Ok(epfd) = ep::create() else {
        rt::eprintln!("milkd: epoll_create1 failed");
        return 1;
    };
    let _ = sys::mkdir("/run", 0o755);
    let _ = sys::unlink(SOCK);
    let listener =
        unix::socket(unix::SOCK_STREAM | unix::SOCK_CLOEXEC | unix::SOCK_NONBLOCK).unwrap_or(-1);
    if listener >= 0 && unix::bind(listener, &Name::Path(SOCK)).is_ok() {
        let _ = unix::listen(listener, 64);
        // Anyone may connect; what they may do is decided per request (SO_PEERCRED).
        let _ = sys::chmod(SOCK, 0o666);
        let _ = ep::add(epfd, listener, EPOLLIN, TAG_LISTENER);
    }
    let mut m = Milkd {
        units: BTreeMap::new(),
        ep: epfd,
        journal,
        listener,
        clients: BTreeMap::new(),
        next_id: 0,
    };
    m.note(&alloc::format!(
        "milkd (pid {}) up, control socket {SOCK}",
        sys::getpid()
    ));
    m.load();
    m.schedule();
    loop {
        let evs = ep::wait(m.ep, 64, m.next_timeout()).unwrap_or_default();
        for ev in evs {
            let (tag, idx) = (ev.data & !0xffff_ffff, ev.data & 0xffff_ffff);
            match tag {
                TAG_LISTENER => {
                    while let Ok(c) = unix::accept(m.listener, unix::SOCK_CLOEXEC) {
                        let _ = ep::add(m.ep, c, EPOLLIN, TAG_CLIENT | c as u64);
                    }
                }
                TAG_CLIENT => m.client_readable(idx as i32),
                TAG_OUT | TAG_ERR => {
                    if let Some(n) = m.by_index(idx) {
                        m.pump(&n, tag == TAG_ERR);
                    }
                }
                TAG_ACTIVATE => {
                    if let Some(n) = m.by_index(idx) {
                        let fd = m.units[&n].listen_fd;
                        let _ = ep::del(m.ep, fd);
                        m.note(&alloc::format!("{n}: first connection, starting"));
                        let user = m.units[&n].def.user.clone();
                        if let Some(acct) = users::by_name(&user) {
                            m.spawn(&n, &acct);
                        }
                    }
                }
                _ => {}
            }
        }
        m.tick();
    }
}
