//! Each command's `help`, in the format `files help` uses: a usage line, then one line per
//! subcommand (two spaces in, the description from column 26), `help` last, then a note.

pub const PROCESSES: &str = "\
usage: processes COMMAND ...    what's running, and stopping it

  list                    every process: PID, parent, state, CPU time, memory, name
  watch                   a live view: CPUs, memory, network, disk, the busiest first
  stop NAME|PID           ask a process to stop (TERM); --now stops it at once (KILL)
  signal NAME|PID SIGNAL  send it any signal: HUP, INT, USR1, ... or a number
  signals                 the signals and what they mean
  info NAME|PID           one process: command, owner, memory, CPU, sandbox, ticket
  help                    this list

A NAME more than one process has is refused with their PIDs (--all means every one);
--group PGID stops or signals a whole process group. list --kernel adds the kernel's own
threads. In watch, q quits, c, m and p sort by CPU, memory and PID, + and - refresh faster
and slower; watch --plain [--frames N] [--every SECS] prints plain text (for logs).
";

pub const SERVICES: &str = "\
usage: services COMMAND ...    background services, which milkd starts and watches

  list                    every service: state, PID, restarts, description
  status [NAME]           one service in detail (no NAME: all of them)
  start NAME              start a service
  stop NAME               stop it (TERM, then KILL after 3 seconds)
  restart NAME            stop it and start it again
  signal NAME [SIGNAL]    signal its process (default KILL); its restart policy applies
  reload                  read the service files in /System/Settings/milk/services again
  logs [NAME...]          the journal: --follow, --since 10m|2h|1d, --last N
  help                    this list

start, stop, restart, signal and reload are for administrators (admin services ...), and
so is logs. logs also takes --level err|warn|info and --this-boot. list and status give
msh a table when asked.
";

pub const SYSTEM: &str = "\
usage: system COMMAND ...    this computer

  info                    name, version, kernel, uptime, CPUs and memory
  memory                  where the memory goes, in Activity Monitor's groups
  uptime                  how long since it started, and how busy it has been
  help                    this list

Restart and Shut Down are in the milk menu on the desktop: the shell can't do them yet.
";

pub const USERS: &str = "\
usage: users COMMAND ...    the accounts on this computer

  me                      who you are: name, id, groups
  list                    every account: name, id, groups, home, shell
  password [NAME]         change your password (root can change anyone's)
  switch [NAME]           a shell as NAME (default root), after NAME's password
  help                    this list

switch --command COMMAND runs one command instead of a shell. Root needs no password.
list gives msh a table when asked.
";

pub const ADMIN: &str = "\
usage: admin COMMAND ...    run a command as an administrator

  COMMAND...              run it as root, after your own password (group admin only)
  run COMMAND...          the same, for a command called help, status, ticket or run
  ticket COMMAND...       run it as you, with a 5-minute administrator's ticket
  status                  are you an administrator, and does this process have a ticket
  help                    this list

--user NAME before the command runs it as NAME instead of root; with ticket it is whose
password to ask for. Every admin asks for the password: nothing is remembered. A ticket
is what snapshot rollbacks, deletes and disk passphrases ask for (docs/security.md); it
ends with its command.
";

/// The subcommands a help text lists (the first word of each entry line).
pub fn verbs(help: &str) -> alloc::vec::Vec<&str> {
    help.lines()
        .filter(|l| l.starts_with("  ") && l.len() > 26 && !l[2..3].contains(' '))
        .filter_map(|l| l.split_whitespace().next())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, &str); 5] = [
        ("processes", PROCESSES),
        ("services", SERVICES),
        ("system", SYSTEM),
        ("users", USERS),
        ("admin", ADMIN),
    ];

    #[test]
    fn same_format_as_files_help() {
        for (noun, h) in ALL {
            let first = h.lines().next().unwrap();
            assert!(
                first.starts_with(&alloc::format!("usage: {noun} COMMAND ...    ")),
                "{noun}"
            );
            assert_eq!(h.lines().nth(1), Some(""), "{noun}");
            let entries: alloc::vec::Vec<&str> =
                h.lines().skip(2).take_while(|l| !l.is_empty()).collect();
            assert!(entries.len() >= 4, "{noun}");
            for e in &entries {
                assert!(e.starts_with("  ") && !e[2..3].contains(' '), "{noun}: {e}");
                assert_eq!(&e[24..26], "  ", "{noun}: description at column 26: {e}");
                assert_ne!(&e[26..27], " ", "{noun}: {e}");
                assert!(e.len() <= 92, "{noun}: too long: {e}");
            }
            assert_eq!(
                *entries.last().unwrap(),
                "  help                    this list"
            );
            for l in h.lines() {
                assert!(l.len() <= 92, "{noun}: {l}");
            }
        }
    }

    #[test]
    fn every_verb_is_listed_and_parses() {
        use crate::args;
        assert_eq!(
            verbs(PROCESSES),
            ["list", "watch", "stop", "signal", "signals", "info", "help"]
        );
        assert_eq!(
            verbs(SERVICES),
            [
                "list", "status", "start", "stop", "restart", "signal", "reload", "logs", "help"
            ]
        );
        assert_eq!(verbs(SYSTEM), ["info", "memory", "uptime", "help"]);
        assert_eq!(verbs(USERS), ["me", "list", "password", "switch", "help"]);
        assert_eq!(
            verbs(ADMIN),
            ["COMMAND...", "run", "ticket", "status", "help"]
        );
        for v in verbs(PROCESSES) {
            let e = args::processes(&[v]);
            assert!(
                !e.as_ref()
                    .is_err_and(|u| u.0.contains("there's no processes")),
                "{v}"
            );
        }
        for v in verbs(SERVICES) {
            let w = [v];
            let e = args::services(&w);
            assert!(
                !e.as_ref()
                    .is_err_and(|u| u.0.contains("there's no services")),
                "{v}"
            );
        }
        for v in verbs(SYSTEM) {
            assert!(args::system(&[v]).is_ok(), "{v}");
        }
        for v in verbs(USERS) {
            assert!(args::users(&[v]).is_ok(), "{v}");
        }
        for w in args::OWN_WORDS {
            assert!(verbs(ADMIN).contains(&w), "{w}");
        }
    }
}
