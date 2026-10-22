//! Signal names and numbers (Linux aarch64 numbering, the ones milkOS delivers).

pub const TERM: i32 = 15;
pub const KILL: i32 = 9;

/// (number, name without SIG, what it usually means).
pub const ALL: [(i32, &str, &str); 20] = [
    (1, "HUP", "hang up: many services read their settings again"),
    (2, "INT", "interrupt, like Ctrl-C"),
    (3, "QUIT", "quit"),
    (4, "ILL", "illegal instruction"),
    (5, "TRAP", "trace trap"),
    (6, "ABRT", "abort"),
    (7, "BUS", "bus error"),
    (8, "FPE", "arithmetic error"),
    (9, "KILL", "stop at once (can't be caught)"),
    (10, "USR1", "user-defined 1"),
    (11, "SEGV", "bad memory access"),
    (12, "USR2", "user-defined 2"),
    (13, "PIPE", "write to a pipe nobody reads"),
    (14, "ALRM", "alarm clock"),
    (15, "TERM", "please stop (the polite one)"),
    (17, "CHLD", "a child process stopped or exited"),
    (18, "CONT", "carry on after STOP"),
    (19, "STOP", "pause (can't be caught)"),
    (20, "TSTP", "pause, like Ctrl-Z"),
    (28, "WINCH", "the terminal changed size"),
];

/// "TERM" for 15; `None` for a number milkOS has no name for.
pub fn name(n: i32) -> Option<&'static str> {
    ALL.iter().find(|s| s.0 == n).map(|s| s.1)
}

/// `TERM`, `term`, `SIGTERM` or `15` (any number 1..=31) as a signal number.
pub fn number(s: &str) -> Option<i32> {
    if let Ok(n) = s.parse::<i32>() {
        return (1..=31).contains(&n).then_some(n);
    }
    let up = s.to_ascii_uppercase();
    let bare = up.strip_prefix("SIG").unwrap_or(&up);
    ALL.iter().find(|x| x.1 == bare).map(|x| x.0)
}

/// "TERM" or, for one without a name, "signal 30".
pub fn describe(n: i32) -> alloc::string::String {
    match name(n) {
        Some(s) => alloc::string::String::from(s),
        None => alloc::format!("signal {n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_numbers() {
        assert_eq!(number("TERM"), Some(15));
        assert_eq!(number("term"), Some(15));
        assert_eq!(number("SIGKILL"), Some(9));
        assert_eq!(number("sigusr1"), Some(10));
        assert_eq!(number("9"), Some(9));
        assert_eq!(number("31"), Some(31));
        assert_eq!(number("0"), None);
        assert_eq!(number("32"), None);
        assert_eq!(number("-1"), None);
        assert_eq!(number("NOPE"), None);
        assert_eq!(number(""), None);
        assert_eq!(name(28), Some("WINCH"));
        assert_eq!(name(30), None);
        assert_eq!(describe(1), "HUP");
        assert_eq!(describe(30), "signal 30");
        for (n, s, _) in ALL {
            assert_eq!(number(s), Some(n));
        }
    }
}
