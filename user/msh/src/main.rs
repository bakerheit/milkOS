//! msh, the milk shell.
//!
//! * `a | b | c`, `> >> < 2> 2>&1`, `&&`, `||`, `;`, `&` (background job)
//! * `$VAR`, `${VAR}`, `$?`, `$$`, `~`, '...' and "..." quoting, `\` escapes, `*`/`?` globs
//! * `NAME=value` (shell variable), `export NAME[=value]`, `unset`, `NAME=value cmd`
//! * Job control: each pipeline runs in its own process group, which gets the terminal
//!   while it runs, so Ctrl-C kills the job and not the shell. `jobs`, `fg [%N]`, `wait`.
//! * Built-ins: cd pwd exit export unset set jobs fg wait help source
//! * Everything else is a program found on $PATH (the coreutils live in /Commands).
//! * `msh -c 'commands'` and `msh FILE` run non-interactively.

#![no_std]
#![no_main]

extern crate alloc;

mod lex;
mod parse;
mod tablecmd;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use parse::{AndOr, Item, Join, Pipeline};
use rt::{eprintln, print, println, sys};

use lex::{Part, RedirKind, Word};

rt::main!(main);

struct Var {
    value: String,
    exported: bool,
}

struct Job {
    id: usize,
    pgid: u32,
    /// Children still running.
    pids: Vec<u32>,
    /// Wait status of the last stage, once it is known.
    last_pid: u32,
    status: Option<i32>,
    text: String,
}

/// One pipeline stage, expanded.
#[derive(Default)]
struct Stage {
    argv: Vec<String>,
    assigns: Vec<(String, String)>,
    redirs: Vec<parse::Redir>,
}

struct Shell {
    vars: BTreeMap<String, Var>,
    last: i32,
    pgid: u32,
    interactive: bool,
    jobs: Vec<Job>,
    next_job: usize,
}

static mut GOT_SIGINT: bool = false;

extern "C" fn on_sigint(_: i32) {
    // SAFETY: single-threaded; read and cleared by the main loop.
    unsafe { GOT_SIGINT = true };
}

fn main(args: &rt::Args) -> i32 {
    let mut sh = Shell {
        vars: BTreeMap::new(),
        last: 0,
        pgid: sys::getpid(),
        interactive: false,
        jobs: Vec::new(),
        next_job: 1,
    };
    for e in &args.envp {
        if let Some((k, v)) = e.split_once('=') {
            sh.set(k, v, true);
        }
    }
    if sh.get("PATH").is_empty() {
        sh.set("PATH", "/Commands", true);
    }
    match (args.get(1), args.get(2)) {
        (Some("-c"), Some(cmd)) => {
            sh.run_line(cmd);
            return sh.last;
        }
        (Some(file), _) if !file.starts_with('-') => return sh.source(file),
        _ => {}
    }

    // Interactive: lead our own process group and own the terminal.
    sh.interactive = sys::isatty(0);
    if sh.interactive {
        if sys::getpgid(0).unwrap_or(0) != sh.pgid {
            let _ = sys::setpgid(0, 0);
        }
        sh.pgid = sys::getpgid(0).unwrap_or(sh.pgid);
        let _ = sys::tcsetpgrp(0, sh.pgid);
        let _ = sys::signal(sys::SIGINT, on_sigint);
        let _ = sys::signal(sys::SIGQUIT, on_sigint);
        println!("msh: milk shell. 'help' for built-ins; programs live in /Commands.");
    }
    let mut line = Vec::new();
    loop {
        sh.reap_jobs(true);
        if sh.interactive {
            print!("{}", sh.prompt());
        }
        line.clear();
        match read_line(&mut line) {
            Line::Eof => {
                if sh.interactive {
                    println!();
                    println!("msh: end of input, bye");
                }
                return sh.last;
            }
            Line::Interrupted => {
                // ^C at the prompt: the tty already echoed "^C" and dropped the line.
                sh.last = 130;
                continue;
            }
            Line::Ok => {}
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        if let Some(code) = sh.run_line(&text) {
            return code;
        }
    }
}

enum Line {
    Ok,
    Eof,
    Interrupted,
}

/// Read one line from fd 0 (the tty delivers whole lines in cooked mode).
fn read_line(out: &mut Vec<u8>) -> Line {
    let mut buf = [0u8; 1];
    loop {
        // SAFETY: plain static written by the signal handler.
        if unsafe { core::ptr::read_volatile(&raw const GOT_SIGINT) } {
            unsafe { GOT_SIGINT = false };
            out.clear();
            return Line::Interrupted;
        }
        match sys::read(0, &mut buf) {
            Ok(0) => {
                return if out.is_empty() { Line::Eof } else { Line::Ok };
            }
            Ok(_) => {
                if buf[0] == b'\n' {
                    return Line::Ok;
                }
                out.push(buf[0]);
            }
            Err(e) if e.0 == 4 => {} // EINTR: loop checks the flag
            Err(e) if e.0 == 11 => sys::sleep_ms(10),
            Err(_) => return Line::Eof,
        }
    }
}

const BUILTINS: &[&str] = &[
    "cd", "pwd", "exit", "export", "unset", "set", "jobs", "fg", "wait", "help", "source", ".",
];

impl Shell {
    fn get(&self, k: &str) -> String {
        self.vars
            .get(k)
            .map(|v| v.value.clone())
            .unwrap_or_default()
    }

    fn set(&mut self, k: &str, v: &str, export: bool) {
        let e = self.vars.get(k).is_some_and(|x| x.exported);
        self.vars.insert(
            String::from(k),
            Var {
                value: String::from(v),
                exported: export || e,
            },
        );
    }

    fn env(&self, extra: &[(String, String)]) -> Vec<String> {
        let mut env: Vec<String> = self
            .vars
            .iter()
            .filter(|(k, v)| v.exported && !extra.iter().any(|(ek, _)| ek == *k))
            .map(|(k, v)| format!("{k}={}", v.value))
            .collect();
        env.extend(extra.iter().map(|(k, v)| format!("{k}={v}")));
        env
    }

    fn prompt(&self) -> String {
        let cwd = sys::getcwd().unwrap_or_else(|_| String::from("?"));
        let home = self.get("HOME");
        let cwd = if !home.is_empty() && cwd.starts_with(&home) {
            format!("~{}", &cwd[home.len()..])
        } else {
            cwd
        };
        format!("msh:{cwd}$ ")
    }

    /// Run one line. Some(code) means "exit the shell with code".
    fn run_line(&mut self, text: &str) -> Option<i32> {
        let items = match lex::lex(text).and_then(parse::parse) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("msh: {e}");
                self.last = 2;
                return None;
            }
        };
        for item in items {
            if let Some(code) = self.run_item(&item, text) {
                return Some(code);
            }
        }
        None
    }

    fn source(&mut self, file: &str) -> i32 {
        let Ok(data) = sys::read_file(file) else {
            eprintln!("msh: {file}: cannot read");
            return 127;
        };
        for l in String::from_utf8_lossy(&data).lines() {
            if let Some(code) = self.run_line(l) {
                return code;
            }
        }
        self.last
    }

    fn run_item(&mut self, item: &Item, text: &str) -> Option<i32> {
        if item.background {
            if item.job.rest.is_empty() {
                self.run_pipeline(&item.job.first, true, text);
            } else {
                // `a && b &`: the whole list runs in a background subshell.
                match sys::fork() {
                    Ok(0) => {
                        let _ = sys::setpgid(0, 0);
                        self.interactive = false;
                        let code = match self.run_andor(&item.job, text) {
                            Some(c) => c,
                            None => self.last,
                        };
                        sys::exit(code)
                    }
                    Ok(pid) => {
                        let _ = sys::setpgid(pid, pid);
                        self.add_job(pid, alloc::vec![pid], pid, text);
                    }
                    Err(e) => eprintln!("msh: fork: {e}"),
                }
            }
            return None;
        }
        self.run_andor(&item.job, text)
    }

    fn run_andor(&mut self, a: &AndOr, text: &str) -> Option<i32> {
        if let Some(c) = self.run_pipeline(&a.first, false, text) {
            return Some(c);
        }
        for (join, p) in &a.rest {
            let go = match join {
                Join::And => self.last == 0,
                Join::Or => self.last != 0,
            };
            if go && let Some(c) = self.run_pipeline(p, false, text) {
                return Some(c);
            }
        }
        None
    }

    // ---- expansion ------------------------------------------------------------------

    fn expand_var(&self, name: &str) -> String {
        match name {
            "?" => format!("{}", self.last),
            "$" => format!("{}", sys::getpid()),
            "0" => String::from("msh"),
            n => self.get(n),
        }
    }

    /// Expand a word to zero or more fields (only unquoted globs produce several).
    fn expand(&self, w: &Word) -> Vec<String> {
        let mut s = String::new();
        let mut glob = false;
        // Glob chars are only special when unquoted; keep a mask of which ones.
        let mut pattern = String::new();
        for p in w {
            match p {
                Part::Lit(t, quoted) => {
                    s.push_str(t);
                    if !quoted && (t.contains('*') || t.contains('?')) {
                        glob = true;
                        pattern.push_str(t);
                    } else {
                        pattern.push_str(&glob_escape(t));
                    }
                }
                Part::Var(n) => {
                    let v = self.expand_var(n);
                    pattern.push_str(&glob_escape(&v));
                    s.push_str(&v);
                }
                Part::Home => {
                    let h = self.get("HOME");
                    pattern.push_str(&glob_escape(&h));
                    s.push_str(&h);
                }
            }
        }
        if glob {
            let m = glob_expand(&pattern);
            if !m.is_empty() {
                return m;
            }
        }
        alloc::vec![s]
    }

    fn expand_words(&self, ws: &[Word]) -> Vec<String> {
        ws.iter().flat_map(|w| self.expand(w)).collect()
    }

    fn expand_one(&self, w: &Word) -> String {
        self.expand(w).join(" ")
    }

    // ---- pipelines -------------------------------------------------------------------

    /// Run a pipeline; sets `self.last`. Some(code) = `exit` was run.
    fn run_pipeline(&mut self, p: &Pipeline, background: bool, text: &str) -> Option<i32> {
        // A lone built-in or bare assignment runs in the shell itself.
        if p.len() == 1 && !background {
            let c = &p[0];
            let argv = self.expand_words(&c.words);
            if argv.is_empty() {
                for (k, v) in &c.assigns {
                    let v = self.expand_one(v);
                    self.set(k, &v, false);
                }
                // Redirections alone (`> file`) still create the file.
                let saved = self.apply_redirs(&c.redirs, true);
                self.restore(saved);
                self.last = 0;
                return None;
            }
            if BUILTINS.contains(&argv[0].as_str()) {
                let saved = self.apply_redirs(&c.redirs, true);
                let Some(saved) = saved else {
                    self.last = 1;
                    return None;
                };
                let r = self.builtin(&argv);
                self.restore(Some(saved));
                return r;
            }
        }

        let stages = self.plan(p);
        let n = stages.len();
        let mut pids = Vec::new();
        let mut pgid = 0u32;
        let mut prev_read: Option<i32> = None;
        for (i, st) in stages.iter().enumerate() {
            let pipe = if i + 1 < n {
                match sys::pipe() {
                    Ok(fds) => Some(fds),
                    Err(e) => {
                        eprintln!("msh: pipe: {e}");
                        break;
                    }
                }
            } else {
                None
            };
            let (argv, assigns) = (&st.argv, &st.assigns);
            match sys::fork() {
                Ok(0) => {
                    // Child: own process group (the pipeline's), default signals, plumbing.
                    let _ = sys::setpgid(0, pgid);
                    let _ = sys::sigaction(sys::SIGINT, sys::SIG_DFL);
                    let _ = sys::sigaction(sys::SIGQUIT, sys::SIG_DFL);
                    if let Some(r) = prev_read {
                        let _ = sys::dup2(r, 0);
                        let _ = sys::close(r);
                    }
                    if let Some((r, w)) = pipe {
                        let _ = sys::dup2(w, 1);
                        let _ = (sys::close(r), sys::close(w));
                    }
                    if self.apply_redirs(&st.redirs, false).is_none() {
                        sys::exit(1);
                    }
                    sys::exit(self.exec_child(argv, assigns));
                }
                Ok(pid) => {
                    if pgid == 0 {
                        pgid = pid;
                    }
                    let _ = sys::setpgid(pid, pgid); // both sides set it: no race
                    pids.push(pid);
                }
                Err(e) => {
                    eprintln!("msh: fork: {e}");
                    break;
                }
            }
            if let Some(r) = prev_read.take() {
                let _ = sys::close(r);
            }
            if let Some((r, w)) = pipe {
                let _ = sys::close(w);
                prev_read = Some(r);
            }
        }
        if let Some(r) = prev_read {
            let _ = sys::close(r);
        }
        if pids.is_empty() {
            self.last = 1;
            return None;
        }
        let last_pid = *pids.last().unwrap();
        if background {
            self.add_job(pgid, pids, last_pid, text);
            self.last = 0;
            return None;
        }
        self.wait_foreground(pgid, pids, last_pid, text);
        None
    }

    /// Give the terminal to `pgid`, wait for `pids`, take the terminal back.
    fn wait_foreground(&mut self, pgid: u32, mut pids: Vec<u32>, last_pid: u32, text: &str) {
        if self.interactive {
            let _ = sys::tcsetpgrp(0, pgid);
        }
        let mut last_status = 0;
        while !pids.is_empty() {
            match sys::wait4(-(pgid as i32), 0) {
                Ok((pid, st)) => {
                    pids.retain(|&p| p != pid);
                    if pid == last_pid {
                        last_status = st;
                    }
                }
                Err(e) if e.0 == 4 => {} // EINTR (SIGCHLD etc.)
                Err(_) => break,
            }
        }
        if self.interactive {
            let _ = sys::tcsetpgrp(0, self.pgid);
        }
        // SAFETY: plain static; a ^C meant for the job may also have hit us.
        unsafe { GOT_SIGINT = false };
        self.last = status_code(last_status);
        if last_status & 0x7f != 0 {
            let sig = last_status & 0x7f;
            // ^C and broken pipes are expected; anything else is worth a word.
            if sig != sys::SIGINT && sig != sys::SIGPIPE {
                let cmd = text.split_whitespace().next().unwrap_or("");
                println!("msh: {cmd}: {}", sys::describe_status(last_status));
            }
        }
    }

    fn add_job(&mut self, pgid: u32, pids: Vec<u32>, last_pid: u32, text: &str) {
        let id = self.next_job;
        self.next_job += 1;
        let text = String::from(text.trim().trim_end_matches('&').trim());
        if self.interactive {
            println!("[{id}] {pgid}");
        }
        self.jobs.push(Job {
            id,
            pgid,
            pids,
            last_pid,
            status: None,
            text,
        });
    }

    /// Collect finished background children; report finished jobs.
    fn reap_jobs(&mut self, report: bool) {
        while let Ok((pid, st)) = sys::wait4(-1, sys::WNOHANG) {
            if pid == 0 {
                break;
            }
            for j in self.jobs.iter_mut() {
                if j.pids.contains(&pid) {
                    j.pids.retain(|&p| p != pid);
                    if pid == j.last_pid {
                        j.status = Some(st);
                    }
                }
            }
        }
        let interactive = self.interactive;
        self.jobs.retain(|j| {
            if !j.pids.is_empty() {
                return true;
            }
            if report && interactive {
                let st = j.status.unwrap_or(0);
                let what = if st == 0 {
                    String::from("Done")
                } else {
                    sys::describe_status(st)
                };
                println!("[{}] {what}  {}", j.id, j.text);
            }
            false
        });
    }

    // ---- redirections --------------------------------------------------------------

    /// Apply redirections. With `save`, return (fd, saved copy) pairs to restore later.
    /// None if one failed (already reported).
    fn apply_redirs(&self, rs: &[parse::Redir], save: bool) -> Option<Vec<(i32, i32)>> {
        let mut saved = Vec::new();
        for r in rs {
            let target = self.expand_one(&r.target);
            let newfd = if r.kind == RedirKind::Dup {
                match target.parse::<i32>() {
                    Ok(fd) => fd,
                    Err(_) => {
                        eprintln!("msh: {target}: bad file descriptor");
                        self.restore(Some(saved));
                        return None;
                    }
                }
            } else {
                let flags = match r.kind {
                    RedirKind::In => sys::O_RDONLY,
                    RedirKind::Out => sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC,
                    _ => sys::O_WRONLY | sys::O_CREAT | sys::O_APPEND,
                };
                match sys::open_mode(&target, flags | sys::O_CLOEXEC, 0o644) {
                    Ok(fd) => fd,
                    Err(e) => {
                        eprintln!("msh: {target}: {e}");
                        self.restore(Some(saved));
                        return None;
                    }
                }
            };
            if save {
                // Keep a copy of the original fd above the user range.
                if let Ok(copy) = sys::fcntl_dupfd(r.fd, 10) {
                    saved.push((r.fd, copy));
                } else {
                    saved.push((r.fd, -1));
                }
            }
            if let Err(e) = sys::dup2(newfd, r.fd) {
                eprintln!("msh: redirect: {e}");
            }
            if r.kind != RedirKind::Dup {
                let _ = sys::close(newfd);
            }
        }
        Some(saved)
    }

    fn restore(&self, saved: Option<Vec<(i32, i32)>>) {
        for (fd, copy) in saved.unwrap_or_default().into_iter().rev() {
            if copy >= 0 {
                let _ = sys::dup2(copy, fd);
                let _ = sys::close(copy);
            } else {
                let _ = sys::close(fd);
            }
        }
    }

    // ---- running things -----------------------------------------------------------

    fn find(&self, name: &str) -> Option<String> {
        if name.contains('/') {
            return Some(String::from(name));
        }
        self.get("PATH")
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| format!("{}/{name}", d.trim_end_matches('/')))
            .find(|p| sys::stat_path(p, true).is_ok_and(|s| !s.is_dir()))
    }

    /// Expand a pipeline into stages and work out structured pipes (docs/shell.md):
    /// a stage feeding a table command gets MILK_TABLE=1; wherever a table would reach
    /// something that isn't a table command (or the end), a `table` renderer goes in,
    /// taking over the stage's stdout redirections.
    fn plan(&self, p: &Pipeline) -> Vec<Stage> {
        let mut stages: Vec<Stage> = p
            .iter()
            .map(|c| Stage {
                argv: self.expand_words(&c.words),
                assigns: c
                    .assigns
                    .iter()
                    .map(|(k, v)| (k.clone(), self.expand_one(v)))
                    .collect(),
                redirs: c.redirs.clone(),
            })
            .collect();
        let name = |s: &Stage| s.argv.first().cloned().unwrap_or_default();
        let mut out: Vec<Stage> = Vec::new();
        let n = stages.len();
        for i in 0..n {
            let mut st = core::mem::take(&mut stages[i]);
            let me = name(&st);
            let next_is_table = stages
                .get(i + 1)
                .is_some_and(|s| tablecmd::is_table_cmd(&name(s)));
            if next_is_table && !tablecmd::is_table_cmd(&me) {
                st.assigns
                    .push((String::from("MILK_TABLE"), String::from("1")));
            }
            let render = tablecmd::emits_table(&me) && !next_is_table;
            let mut moved = Vec::new();
            if render {
                st.redirs.retain(|r| {
                    let stdout = r.fd == 1;
                    if stdout {
                        moved.push(r.clone());
                    }
                    !stdout
                });
            }
            out.push(st);
            if render {
                out.push(Stage {
                    argv: alloc::vec![String::from("table")],
                    assigns: Vec::new(),
                    redirs: moved,
                });
            }
        }
        out
    }

    /// In a forked child: run a built-in or exec a program. Returns the exit code if
    /// it didn't exec.
    fn exec_child(&mut self, argv: &[String], assigns: &[(String, String)]) -> i32 {
        if argv.is_empty() {
            return 0;
        }
        if BUILTINS.contains(&argv[0].as_str()) {
            return match self.builtin(argv) {
                Some(c) => c,
                None => self.last,
            };
        }
        if tablecmd::is_table_cmd(&argv[0]) {
            return tablecmd::run(argv);
        }
        let Some(path) = self.find(&argv[0]) else {
            eprintln!("msh: {}: command not found", argv[0]);
            return 127;
        };
        let env = self.env(assigns);
        let a: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let e: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
        let err = sys::execve(&path, &a, &e);
        eprintln!("msh: {}: {err}", argv[0]);
        if err.0 == 2 { 127 } else { 126 }
    }

    /// Run a built-in. Some(code) = exit the shell.
    fn builtin(&mut self, argv: &[String]) -> Option<i32> {
        let arg = |i: usize| argv.get(i).map(|s| s.as_str());
        let mut rc = 0;
        match argv[0].as_str() {
            "cd" => {
                let home = self.get("HOME");
                let dir = match arg(1) {
                    Some("-") => self.get("OLDPWD"),
                    Some(d) => String::from(d),
                    None => home,
                };
                let old = sys::getcwd().unwrap_or_default();
                match sys::chdir(&dir) {
                    Ok(()) => {
                        self.set("OLDPWD", &old, true);
                        let new = sys::getcwd().unwrap_or_default();
                        self.set("PWD", &new, true);
                    }
                    Err(e) => {
                        eprintln!("cd: {dir}: {e}");
                        rc = 1;
                    }
                }
            }
            "pwd" => println!("{}", sys::getcwd().unwrap_or_default()),
            "exit" => {
                let code = arg(1).and_then(|s| s.parse().ok()).unwrap_or(self.last);
                return Some(code);
            }
            "export" => {
                if argv.len() == 1 {
                    for (k, v) in self.vars.iter().filter(|(_, v)| v.exported) {
                        println!("export {k}={}", v.value);
                    }
                }
                for a in &argv[1..] {
                    match a.split_once('=') {
                        Some((k, v)) => self.set(k, v, true),
                        None => {
                            let v = self.get(a);
                            self.set(a, &v, true);
                        }
                    }
                }
            }
            "unset" => {
                for a in &argv[1..] {
                    self.vars.remove(a.as_str());
                }
            }
            "set" => {
                for (k, v) in &self.vars {
                    println!("{k}={}", v.value);
                }
            }
            "jobs" => {
                self.reap_jobs(true);
                for j in &self.jobs {
                    println!("[{}] Running  {}  (pgid {})", j.id, j.text, j.pgid);
                }
            }
            "fg" => {
                self.reap_jobs(false);
                let want = arg(1).and_then(|s| s.trim_start_matches('%').parse::<usize>().ok());
                let idx = match want {
                    Some(id) => self.jobs.iter().position(|j| j.id == id),
                    None => self.jobs.len().checked_sub(1),
                };
                match idx {
                    Some(i) => {
                        let j = self.jobs.remove(i);
                        println!("{}", j.text);
                        let text = j.text.clone();
                        self.wait_foreground(j.pgid, j.pids, j.last_pid, &text);
                        rc = self.last;
                    }
                    None => {
                        eprintln!("fg: no such job");
                        rc = 1;
                    }
                }
            }
            "wait" => {
                while self.jobs.iter().any(|j| !j.pids.is_empty()) {
                    match sys::wait4(-1, 0) {
                        Ok((pid, st)) => {
                            for j in self.jobs.iter_mut() {
                                j.pids.retain(|&p| p != pid);
                                if pid == j.last_pid {
                                    j.status = Some(st);
                                }
                            }
                        }
                        Err(e) if e.0 == 4 => {
                            if unsafe { GOT_SIGINT } {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                self.reap_jobs(true);
            }
            "source" | "." => match arg(1) {
                Some(f) => rc = self.source(f),
                None => {
                    eprintln!("source: FILE needed");
                    rc = 2;
                }
            },
            "help" => help(),
            _ => rc = 127,
        }
        self.last = rc;
        None
    }
}

fn status_code(st: i32) -> i32 {
    if st & 0x7f == 0 {
        (st >> 8) & 0xff
    } else {
        128 + (st & 0x7f)
    }
}

fn help() {
    println!("msh, the milk shell");
    println!("  a | b | c        pipeline        a && b, a || b, a; b, a &   lists");
    println!("  > f, >> f, < f, 2> f, 2>&1       redirection");
    println!("  $VAR ${{VAR}} $? $$ ~ * ?        expansion; '...' and \"...\" quote");
    println!("  NAME=v, export NAME[=v], unset NAME, set     variables");
    println!("  cd [DIR|-], pwd, exit [N], jobs, fg [%N], wait, source FILE, help");
    println!("  Ctrl-C stops the running job; Ctrl-D at an empty prompt leaves.");
    println!("programs (/Commands): files trash cat echo wc head tail grep sort uniq ps kill");
    println!("  sleep env uptime free uname true false clear   (files help: files and folders)");
    println!("  test tools, not on PATH: /System/Developer (hello fault utest ...)");
    println!("structured pipes (docs/shell.md): where COL OP V, sort-by COL [-r], select COL..,");
    println!(
        "  first N, last N, get COL, count, table, to-json; producers: ps, files list, trash list, env"
    );
    println!(
        "try: ps | grep msh,  files list -l /Commands | sort -n -k5 | tail -3,  cat /System/Settings/motd | wc -l"
    );
    println!(
        "     ps | where rss > 100 | sort-by name,  files list /Commands | sort-by size -r | first 3"
    );
}

// ---- globbing --------------------------------------------------------------------------

/// Protect literal `*` and `?` (from quotes or variables) in a glob pattern.
fn glob_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if c == '*' || c == '?' || c == '\\' {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

fn glob_match(pat: &[char], name: &[char]) -> bool {
    match pat.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|i| glob_match(&pat[1..], &name[i..])),
        Some('?') => !name.is_empty() && glob_match(&pat[1..], &name[1..]),
        Some('\\') if pat.len() > 1 => {
            name.first() == Some(&pat[1]) && glob_match(&pat[2..], &name[1..])
        }
        Some(c) => name.first() == Some(c) && glob_match(&pat[1..], &name[1..]),
    }
}

fn unescape(s: &str) -> String {
    let mut o = String::new();
    let mut esc = false;
    for c in s.chars() {
        if !esc && c == '\\' {
            esc = true;
            continue;
        }
        esc = false;
        o.push(c);
    }
    o
}

/// Expand a pattern whose globs are in the last path component. Sorted; empty if no match.
fn glob_expand(pattern: &str) -> Vec<String> {
    let (dir, file) = match pattern.rfind('/') {
        Some(i) => (&pattern[..i], &pattern[i + 1..]),
        None => ("", pattern),
    };
    let dir_real = unescape(dir);
    let list_dir = if dir.is_empty() {
        String::from(".")
    } else if dir_real.is_empty() {
        String::from("/")
    } else {
        dir_real.clone()
    };
    let pat: Vec<char> = file.chars().collect();
    let mut out: Vec<String> = sys::read_dir(&list_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| n != "." && n != "..")
        .filter(|n| !n.starts_with('.') || file.starts_with('.'))
        .filter(|n| glob_match(&pat, &n.chars().collect::<Vec<_>>()))
        .map(|n| {
            if dir.is_empty() {
                n
            } else {
                format!("{dir_real}/{n}")
            }
        })
        .collect();
    out.sort();
    out
}
