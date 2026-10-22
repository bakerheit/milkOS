//! How each command reads its words. Every error is a whole line to print as it is, and
//! means exit status 2 (a usage error), the way `files` does it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::signals;

/// A usage error: the line to print (to stderr).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage(pub String);

fn no_option(noun: &str, verb: &str, o: &str) -> Usage {
    Usage(format!(
        "{noun} {verb}: there's no option {o} (try {noun} help)"
    ))
}

fn no_verb(noun: &str, verb: &str) -> Usage {
    Usage(format!(
        "{noun}: there's no {noun} {verb} (try {noun} help)"
    ))
}

fn usage(noun: &str, what: &str) -> Usage {
    Usage(format!(
        "usage: {noun} {what}   ({noun} help lists them all)"
    ))
}

fn is_help(a: &str) -> bool {
    matches!(a, "help" | "--help" | "-h")
}

// ---- processes ---------------------------------------------------------------------------

/// Which process(es): a NAME or PID, or a whole group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    One(&'a str),
    Group(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Processes<'a> {
    Help,
    List {
        kernel: bool,
    },
    Watch {
        plain: bool,
        frames: u64,
        every: u64,
    },
    Signal {
        target: Target<'a>,
        signal: i32,
        all: bool,
    },
    Signals,
    Info {
        what: &'a str,
    },
}

/// `processes [list|watch|stop|signal|signals|info|help] ...`; no verb lists.
pub fn processes<'a>(a: &[&'a str]) -> Result<Processes<'a>, Usage> {
    const N: &str = "processes";
    let Some((&verb, rest)) = a.split_first() else {
        return Ok(Processes::List { kernel: false });
    };
    if is_help(verb) || rest.contains(&"--help") {
        return Ok(Processes::Help);
    }
    // Options anywhere; the other words in order.
    let mut words: Vec<&'a str> = Vec::new();
    let (mut now, mut all, mut kernel, mut plain) = (false, false, false, false);
    let (mut frames, mut every, mut group) = (None, None, None);
    let mut it = rest.iter();
    while let Some(&w) = it.next() {
        let number = |v: Option<&&str>, what: &str| -> Result<u64, Usage> {
            v.and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0)
                .ok_or_else(|| Usage(format!("{N} {verb}: {what} wants a number")))
        };
        match (verb, w) {
            ("stop", "--now") => now = true,
            ("stop" | "signal", "--all") => all = true,
            ("stop" | "signal", "--group") => group = Some(number(it.next(), "--group")? as u32),
            ("list", "--kernel") => kernel = true,
            ("watch", "--plain") => plain = true,
            ("watch", "--frames") => frames = Some(number(it.next(), "--frames")?),
            ("watch", "--every") => every = Some(number(it.next(), "--every")?.min(60)),
            (_, "--") => words.extend(it.by_ref().copied()),
            (_, o) if o.starts_with('-') && o.len() > 1 && o.parse::<i64>().is_err() => {
                return Err(if is_verb(verb) {
                    no_option(N, verb, o)
                } else {
                    no_verb(N, verb)
                });
            }
            (_, o) => words.push(o),
        }
    }
    let target = |words: &[&'a str]| -> Option<Target<'a>> {
        match group {
            Some(g) => Some(Target::Group(g)),
            None => words.first().map(|w| Target::One(w)),
        }
    };
    match verb {
        "list" if words.is_empty() => Ok(Processes::List { kernel }),
        "list" => Err(usage(N, "list [--kernel]")),
        "watch" if words.is_empty() => Ok(Processes::Watch {
            plain,
            frames: frames.unwrap_or(if plain { 1 } else { 0 }),
            every: every.unwrap_or(1),
        }),
        "watch" => Err(usage(N, "watch [--plain] [--frames N] [--every SECS]")),
        "stop" => match target(&words) {
            Some(t) if words.len() == usize::from(group.is_none()) => Ok(Processes::Signal {
                target: t,
                signal: if now { signals::KILL } else { signals::TERM },
                all,
            }),
            _ => Err(usage(N, "stop [--now] NAME|PID   (or --group PGID)")),
        },
        "signal" => {
            let need = usize::from(group.is_none()) + 1;
            match (target(&words), words.last()) {
                (Some(t), Some(s)) if words.len() == need => match signals::number(s) {
                    Some(signal) => Ok(Processes::Signal {
                        target: t,
                        signal,
                        all,
                    }),
                    None => Err(Usage(format!(
                        "{N} signal: there's no signal {s} (processes signals lists them)"
                    ))),
                },
                _ => Err(usage(
                    N,
                    "signal NAME|PID SIGNAL   (or --group PGID SIGNAL)",
                )),
            }
        }
        "signals" if words.is_empty() => Ok(Processes::Signals),
        "signals" => Err(usage(N, "signals")),
        "info" if words.len() == 1 => Ok(Processes::Info { what: words[0] }),
        "info" => Err(usage(N, "info NAME|PID")),
        _ => Err(no_verb(N, verb)),
    }
}

fn is_verb(v: &str) -> bool {
    matches!(v, "list" | "watch" | "stop" | "signal" | "signals" | "info")
}

// ---- services ----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Services<'a> {
    Help,
    /// list, or status with no name
    List {
        long: bool,
    },
    /// The request line for milkd: `status N`, `start N`, `stop N`, `restart N`, `reload`.
    Ask {
        verb: &'a str,
        name: Option<&'a str>,
    },
    Signal {
        name: &'a str,
        signal: i32,
    },
    /// The words after `logs`, for journal::parse_args.
    Logs(&'a [&'a str]),
}

/// `services [list|status|start|stop|restart|signal|reload|logs|help] ...`; no verb lists.
pub fn services<'a>(a: &'a [&'a str]) -> Result<Services<'a>, Usage> {
    const N: &str = "services";
    let Some((&verb, rest)) = a.split_first() else {
        return Ok(Services::List { long: false });
    };
    if is_help(verb) || rest.contains(&"--help") {
        return Ok(Services::Help);
    }
    if verb == "logs" {
        return Ok(Services::Logs(rest));
    }
    if let Some(o) = rest.iter().find(|w| w.starts_with('-')) {
        return Err(
            if matches!(
                verb,
                "list" | "status" | "start" | "stop" | "restart" | "signal" | "reload"
            ) {
                no_option(N, verb, o)
            } else {
                no_verb(N, verb)
            },
        );
    }
    match (verb, rest) {
        ("list", []) => Ok(Services::List { long: false }),
        ("status", []) => Ok(Services::List { long: true }),
        ("status" | "start" | "stop" | "restart", [name]) => Ok(Services::Ask {
            verb,
            name: Some(name),
        }),
        ("reload", []) => Ok(Services::Ask { verb, name: None }),
        ("signal", [name]) => Ok(Services::Signal {
            name,
            signal: signals::KILL,
        }),
        ("signal", [name, s]) => match signals::number(s) {
            Some(signal) => Ok(Services::Signal { name, signal }),
            None => Err(Usage(format!(
                "{N} signal: there's no signal {s} (processes signals lists them)"
            ))),
        },
        ("list" | "reload", _) => Err(usage(N, verb)),
        ("status", _) => Err(usage(N, "status [NAME]")),
        ("start" | "stop" | "restart", _) => Err(usage(N, &format!("{verb} NAME"))),
        ("signal", _) => Err(usage(N, "signal NAME [SIGNAL]")),
        _ => Err(no_verb(N, verb)),
    }
}

// ---- system ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    Help,
    Info,
    Memory,
    Uptime,
}

/// `system [info|memory|uptime|help]`; no verb is info.
pub fn system(a: &[&str]) -> Result<System, Usage> {
    const N: &str = "system";
    let Some((&verb, rest)) = a.split_first() else {
        return Ok(System::Info);
    };
    if is_help(verb) || rest.contains(&"--help") {
        return Ok(System::Help);
    }
    let v = match verb {
        "info" => System::Info,
        "memory" => System::Memory,
        "uptime" => System::Uptime,
        "restart" | "shutdown" | "shut-down" => {
            return Err(Usage(format!(
                "{N}: {verb} isn't something the shell can do yet: use the milk menu on the desktop"
            )));
        }
        _ => return Err(no_verb(N, verb)),
    };
    match rest.first() {
        None => Ok(v),
        Some(o) if o.starts_with('-') => Err(no_option(N, verb, o)),
        Some(_) => Err(usage(N, verb)),
    }
}

// ---- users -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Users<'a> {
    Help,
    Me,
    List,
    /// Whose (None: yours).
    Password(Option<&'a str>),
    /// To whom, and one command instead of a shell.
    Switch {
        name: &'a str,
        command: Option<&'a str>,
    },
}

/// `users [me|list|password|switch|help] ...`; no verb is me.
pub fn users<'a>(a: &[&'a str]) -> Result<Users<'a>, Usage> {
    const N: &str = "users";
    let Some((&verb, rest)) = a.split_first() else {
        return Ok(Users::Me);
    };
    if is_help(verb) || rest.contains(&"--help") {
        return Ok(Users::Help);
    }
    match verb {
        "me" | "list" => match rest.first() {
            None => Ok(if verb == "me" { Users::Me } else { Users::List }),
            Some(o) if o.starts_with('-') => Err(no_option(N, verb, o)),
            Some(_) => Err(usage(N, verb)),
        },
        "password" => match rest {
            [] => Ok(Users::Password(None)),
            [o] if o.starts_with('-') => Err(no_option(N, verb, o)),
            [name] => Ok(Users::Password(Some(name))),
            _ => Err(usage(N, "password [NAME]")),
        },
        "switch" => {
            let (mut name, mut command) = (None, None);
            let mut it = rest.iter();
            while let Some(&w) = it.next() {
                match w {
                    "--command" => match it.next() {
                        Some(&c) if command.is_none() => command = Some(c),
                        _ => return Err(usage(N, "switch [NAME] [--command COMMAND]")),
                    },
                    o if o.starts_with('-') => return Err(no_option(N, verb, o)),
                    n if name.is_none() => name = Some(n),
                    _ => return Err(usage(N, "switch [NAME] [--command COMMAND]")),
                }
            }
            Ok(Users::Switch {
                name: name.unwrap_or("root"),
                command,
            })
        }
        _ => Err(no_verb(N, verb)),
    }
}

// ---- admin -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admin<'a> {
    Help,
    Status,
    /// Run `command` as `user` (root unless --user), after your own password (sudo).
    Run {
        user: &'a str,
        command: &'a [&'a str],
    },
    /// Run `command` as you with an administrator's ticket (`admin`: whose password; None =
    /// yours).
    Ticket {
        admin: Option<&'a str>,
        command: &'a [&'a str],
    },
}

/// The words `admin` keeps for itself; anything else first is a command to run. A command with
/// one of these names runs with `admin run NAME` (or its full path).
pub const OWN_WORDS: [&str; 4] = ["help", "status", "ticket", "run"];

/// `admin [--user NAME] [--stdin] COMMAND...`, `admin run ...`, `admin ticket [--user ADMIN]
/// [--stdin] COMMAND...`, `admin status`, `admin help`. Options come before the command:
/// everything from the command on is the command's own.
pub fn admin<'a>(a: &'a [&'a str]) -> Result<Admin<'a>, Usage> {
    const N: &str = "admin";
    let Some((&first, rest)) = a.split_first() else {
        return Ok(Admin::Help);
    };
    let (verb, words) = match first {
        f if is_help(f) => return Ok(Admin::Help),
        "status" if rest.is_empty() => return Ok(Admin::Status),
        "status" => return Err(usage(N, "status")),
        "ticket" | "run" => (first, rest),
        _ => ("run", a),
    };
    let mut user = None;
    let mut i = 0;
    while let Some(&w) = words.get(i) {
        match w {
            "--user" => {
                i += 1;
                match words.get(i) {
                    Some(&u) if !u.starts_with('-') => user = Some(u),
                    _ => return Err(Usage(format!("{N}: --user wants a name"))),
                }
            }
            "--stdin" => {}
            "--" => {
                i += 1;
                break;
            }
            o if o.starts_with('-') => {
                return Err(Usage(format!("{N}: there's no option {o} (try {N} help)")));
            }
            _ => break,
        }
        i += 1;
    }
    let command = &words[i.min(words.len())..];
    if command.is_empty() {
        return Err(if verb == "ticket" {
            usage(N, "ticket [--user ADMIN] COMMAND...")
        } else {
            usage(N, "[--user NAME] COMMAND...")
        });
    }
    Ok(if verb == "ticket" {
        Admin::Ticket {
            admin: user,
            command,
        }
    } else {
        Admin::Run {
            user: user.unwrap_or("root"),
            command,
        }
    })
}

/// Where a command is: a path as given, a bare name in /Commands.
pub fn command_path(prog: &str) -> String {
    if prog.contains('/') {
        String::from(prog)
    } else {
        format!("/Commands/{prog}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_words() {
        use Processes as P;
        assert_eq!(processes(&[]), Ok(P::List { kernel: false }));
        assert_eq!(
            processes(&["list", "--kernel"]),
            Ok(P::List { kernel: true })
        );
        assert_eq!(processes(&["help"]), Ok(P::Help));
        assert_eq!(processes(&["stop", "--help"]), Ok(P::Help));
        assert_eq!(
            processes(&["stop", "msh"]),
            Ok(P::Signal {
                target: Target::One("msh"),
                signal: 15,
                all: false
            })
        );
        assert_eq!(
            processes(&["stop", "--now", "12"]),
            Ok(P::Signal {
                target: Target::One("12"),
                signal: 9,
                all: false
            })
        );
        assert_eq!(
            processes(&["stop", "msh", "--all"]),
            Ok(P::Signal {
                target: Target::One("msh"),
                signal: 15,
                all: true
            })
        );
        assert_eq!(
            processes(&["stop", "--group", "40"]),
            Ok(P::Signal {
                target: Target::Group(40),
                signal: 15,
                all: false
            })
        );
        assert_eq!(
            processes(&["signal", "httpd", "HUP"]),
            Ok(P::Signal {
                target: Target::One("httpd"),
                signal: 1,
                all: false
            })
        );
        assert_eq!(
            processes(&["signal", "--group", "40", "usr1"]),
            Ok(P::Signal {
                target: Target::Group(40),
                signal: 10,
                all: false
            })
        );
        assert_eq!(processes(&["signals"]), Ok(P::Signals));
        assert_eq!(processes(&["info", "1"]), Ok(P::Info { what: "1" }));
        assert_eq!(
            processes(&["watch"]),
            Ok(P::Watch {
                plain: false,
                frames: 0,
                every: 1
            })
        );
        assert_eq!(
            processes(&["watch", "--plain"]),
            Ok(P::Watch {
                plain: true,
                frames: 1,
                every: 1
            })
        );
        assert_eq!(
            processes(&["watch", "--plain", "--frames", "3", "--every", "2"]),
            Ok(P::Watch {
                plain: true,
                frames: 3,
                every: 2
            })
        );
        assert_eq!(
            processes(&["stop"]),
            Err(Usage(String::from(
                "usage: processes stop [--now] NAME|PID   (or --group PGID)   (processes help lists them all)"
            )))
        );
        assert!(processes(&["stop", "a", "b"]).is_err());
        assert!(processes(&["stop", "--group", "4", "a"]).is_err());
        assert_eq!(
            processes(&["signal", "1", "NOPE"]),
            Err(Usage(String::from(
                "processes signal: there's no signal NOPE (processes signals lists them)"
            )))
        );
        assert_eq!(
            processes(&["list", "-k"]),
            Err(Usage(String::from(
                "processes list: there's no option -k (try processes help)"
            )))
        );
        assert_eq!(
            processes(&["kill", "1"]),
            Err(Usage(String::from(
                "processes: there's no processes kill (try processes help)"
            )))
        );
        assert!(processes(&["watch", "--frames", "x"]).is_err());
        assert!(processes(&["watch", "--frames", "0"]).is_err());
        assert!(processes(&["info"]).is_err());
    }

    #[test]
    fn services_words() {
        use Services as S;
        assert_eq!(services(&[]), Ok(S::List { long: false }));
        assert_eq!(services(&["status"]), Ok(S::List { long: true }));
        assert_eq!(
            services(&["status", "httpd"]),
            Ok(S::Ask {
                verb: "status",
                name: Some("httpd")
            })
        );
        assert_eq!(
            services(&["restart", "httpd"]),
            Ok(S::Ask {
                verb: "restart",
                name: Some("httpd")
            })
        );
        assert_eq!(
            services(&["reload"]),
            Ok(S::Ask {
                verb: "reload",
                name: None
            })
        );
        assert_eq!(
            services(&["signal", "rngd"]),
            Ok(S::Signal {
                name: "rngd",
                signal: 9
            })
        );
        assert_eq!(
            services(&["signal", "rngd", "TERM"]),
            Ok(S::Signal {
                name: "rngd",
                signal: 15
            })
        );
        let w = ["logs", "drop", "--follow"];
        assert_eq!(services(&w), Ok(S::Logs(&w[1..])));
        assert_eq!(services(&["start", "--help"]), Ok(S::Help));
        assert_eq!(
            services(&["start"]),
            Err(Usage(String::from(
                "usage: services start NAME   (services help lists them all)"
            )))
        );
        assert!(services(&["start", "a", "b"]).is_err());
        assert!(services(&["list", "-a"]).is_err());
        assert_eq!(
            services(&["kill", "x"]),
            Err(Usage(String::from(
                "services: there's no services kill (try services help)"
            )))
        );
    }

    #[test]
    fn system_words() {
        assert_eq!(system(&[]), Ok(System::Info));
        assert_eq!(system(&["memory"]), Ok(System::Memory));
        assert_eq!(system(&["uptime"]), Ok(System::Uptime));
        assert_eq!(system(&["help"]), Ok(System::Help));
        assert!(system(&["info", "x"]).is_err());
        assert!(system(&["memory", "-h"]).is_err());
        assert!(system(&["restart"]).unwrap_err().0.contains("milk menu"));
        assert_eq!(
            system(&["uname"]),
            Err(Usage(String::from(
                "system: there's no system uname (try system help)"
            )))
        );
    }

    #[test]
    fn users_words() {
        use Users as U;
        assert_eq!(users(&[]), Ok(U::Me));
        assert_eq!(users(&["list"]), Ok(U::List));
        assert_eq!(users(&["password"]), Ok(U::Password(None)));
        assert_eq!(users(&["password", "milk"]), Ok(U::Password(Some("milk"))));
        assert!(users(&["password", "a", "b"]).is_err());
        assert_eq!(
            users(&["switch"]),
            Ok(U::Switch {
                name: "root",
                command: None
            })
        );
        assert_eq!(
            users(&["switch", "milk", "--command", "go /tmp"]),
            Ok(U::Switch {
                name: "milk",
                command: Some("go /tmp")
            })
        );
        assert_eq!(
            users(&["switch", "--command", "x"]),
            Ok(U::Switch {
                name: "root",
                command: Some("x")
            })
        );
        assert!(users(&["switch", "a", "b"]).is_err());
        assert!(users(&["switch", "--command"]).is_err());
        assert_eq!(
            users(&["switch", "-c", "x"]),
            Err(Usage(String::from(
                "users switch: there's no option -c (try users help)"
            )))
        );
        assert!(users(&["whoami"]).is_err());
    }

    #[test]
    fn admin_words() {
        use Admin as A;
        assert_eq!(admin(&[]), Ok(A::Help));
        assert_eq!(admin(&["help"]), Ok(A::Help));
        assert_eq!(admin(&["status"]), Ok(A::Status));
        let w = ["files", "delete", "--now", "/x"];
        assert_eq!(
            admin(&w),
            Ok(A::Run {
                user: "root",
                command: &w[..]
            })
        );
        let w = ["--user", "nobody", "--stdin", "netctl", "visible", "off"];
        assert_eq!(
            admin(&w),
            Ok(A::Run {
                user: "nobody",
                command: &w[3..]
            })
        );
        let w = ["run", "status", "--x"];
        assert_eq!(
            admin(&w),
            Ok(A::Run {
                user: "root",
                command: &w[1..]
            })
        );
        let w = ["ticket", "snap", "delete", "old"];
        assert_eq!(
            admin(&w),
            Ok(A::Ticket {
                admin: None,
                command: &w[1..]
            })
        );
        let w = ["ticket", "--user", "milk", "--", "--odd"];
        assert_eq!(
            admin(&w),
            Ok(A::Ticket {
                admin: Some("milk"),
                command: &w[4..]
            })
        );
        // The command's own options stay the command's.
        let w = ["msh", "--user", "x"];
        assert_eq!(
            admin(&w),
            Ok(A::Run {
                user: "root",
                command: &w[..]
            })
        );
        assert!(admin(&["status", "x"]).is_err());
        assert!(admin(&["ticket"]).is_err());
        assert!(admin(&["run"]).is_err());
        assert!(admin(&["--stdin"]).is_err());
        assert_eq!(
            admin(&["--user"]),
            Err(Usage(String::from("admin: --user wants a name")))
        );
        assert_eq!(
            admin(&["-S", "id"]),
            Err(Usage(String::from(
                "admin: there's no option -S (try admin help)"
            )))
        );
        assert_eq!(command_path("files"), "/Commands/files");
        assert_eq!(
            command_path("/System/Developer/utest"),
            "/System/Developer/utest"
        );
        assert_eq!(command_path("./x"), "./x");
    }
}
