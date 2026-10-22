//! Options for one subcommand: `--long`, `--long VALUE`, `--long=VALUE`, `-s`, `-s VALUE`,
//! several short ones together (`-tl`), `--` to end them. Everything else is kept, in order.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// One option a subcommand takes.
#[derive(Clone, Copy, Debug)]
pub struct Opt {
    pub long: &'static str,
    pub short: Option<char>,
    /// Takes a value (`--count 3`).
    pub value: bool,
}

/// A flag (no value).
pub const fn flag(long: &'static str, short: Option<char>) -> Opt {
    Opt {
        long,
        short,
        value: false,
    }
}

/// An option with a value.
pub const fn valued(long: &'static str, short: Option<char>) -> Opt {
    Opt {
        long,
        short,
        value: true,
    }
}

/// What was given.
#[derive(Debug, Default)]
pub struct Parsed<'a> {
    pub opts: Vec<(&'static str, Option<&'a str>)>,
    pub rest: Vec<&'a str>,
}

impl<'a> Parsed<'a> {
    pub fn has(&self, long: &str) -> bool {
        self.opts.iter().any(|(n, _)| *n == long)
    }
    /// The last value given for `long`.
    pub fn value(&self, long: &str) -> Option<&'a str> {
        self.opts
            .iter()
            .rev()
            .find(|(n, _)| *n == long)
            .and_then(|(_, v)| *v)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Bad {
    /// No such option (as typed).
    Unknown(String),
    /// `--count` with nothing after it.
    NeedsValue(&'static str),
    /// `--headers=yes` on a flag.
    NoValue(&'static str),
}

impl Bad {
    /// "network ping: there's no option --fast (try network help)".
    pub fn words(&self, cmd: &str) -> String {
        let noun = cmd.split(' ').next().unwrap_or(cmd);
        match self {
            Bad::Unknown(o) => format!("{cmd}: there's no option {o} (try {noun} help)"),
            Bad::NeedsValue(o) => format!("{cmd}: --{o} needs a value"),
            Bad::NoValue(o) => format!("{cmd}: --{o} doesn't take a value"),
        }
    }
}

pub fn parse<'a>(args: &[&'a str], spec: &[Opt]) -> Result<Parsed<'a>, Bad> {
    let mut p = Parsed::default();
    let mut done = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        i += 1;
        if done || a == "-" || !a.starts_with('-') {
            p.rest.push(a);
            continue;
        }
        if a == "--" {
            done = true;
            continue;
        }
        if let Some(l) = a.strip_prefix("--") {
            let (name, inline) = match l.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (l, None),
            };
            let o = spec
                .iter()
                .find(|o| o.long == name)
                .ok_or_else(|| Bad::Unknown(String::from(a)))?;
            if !o.value {
                if inline.is_some() {
                    return Err(Bad::NoValue(o.long));
                }
                p.opts.push((o.long, None));
            } else if let Some(v) = inline {
                p.opts.push((o.long, Some(v)));
            } else {
                let v = args.get(i).ok_or(Bad::NeedsValue(o.long))?;
                i += 1;
                p.opts.push((o.long, Some(v)));
            }
            continue;
        }
        // Short ones: `-t`, `-tl`, `-c 3`, `-c3`.
        let body = &a[1..];
        for (k, c) in body.char_indices() {
            let o = spec
                .iter()
                .find(|o| o.short == Some(c))
                .ok_or_else(|| Bad::Unknown(format!("-{c}")))?;
            if !o.value {
                p.opts.push((o.long, None));
                continue;
            }
            let tail = &body[k + c.len_utf8()..];
            if !tail.is_empty() {
                p.opts.push((o.long, Some(tail)));
            } else {
                let v = args.get(i).ok_or(Bad::NeedsValue(o.long))?;
                i += 1;
                p.opts.push((o.long, Some(v)));
            }
            break;
        }
    }
    Ok(p)
}

/// A whole number for `--long`, or the words for a bad one.
pub fn number(cmd: &str, long: &str, v: &str) -> Result<u64, String> {
    v.parse::<u64>()
        .map_err(|_| format!("{cmd}: --{long} wants a whole number (got {v})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &[Opt] = &[
        flag("tcp", Some('t')),
        flag("listening", Some('l')),
        valued("count", Some('c')),
        valued("save", None),
    ];

    #[test]
    fn long_and_short() {
        let p = parse(&["--tcp", "-c", "3", "host", "--save=out.txt"], SPEC).unwrap();
        assert!(p.has("tcp") && !p.has("listening"));
        assert_eq!(p.value("count"), Some("3"));
        assert_eq!(p.value("save"), Some("out.txt"));
        assert_eq!(p.rest, ["host"]);
        let p = parse(&["-tl", "-c5", "--count", "7"], SPEC).unwrap();
        assert!(p.has("tcp") && p.has("listening"));
        assert_eq!(p.value("count"), Some("7"));
        let p = parse(&["--", "--tcp", "-"], SPEC).unwrap();
        assert!(!p.has("tcp"));
        assert_eq!(p.rest, ["--tcp", "-"]);
    }

    #[test]
    fn mistakes() {
        assert_eq!(
            parse(&["--fast"], SPEC).unwrap_err(),
            Bad::Unknown("--fast".into())
        );
        assert_eq!(
            parse(&["-tx"], SPEC).unwrap_err(),
            Bad::Unknown("-x".into())
        );
        assert_eq!(
            parse(&["--count"], SPEC).unwrap_err(),
            Bad::NeedsValue("count")
        );
        assert_eq!(
            parse(&["--tcp=yes"], SPEC).unwrap_err(),
            Bad::NoValue("tcp")
        );
        assert_eq!(
            Bad::Unknown("--fast".into()).words("network ping"),
            "network ping: there's no option --fast (try network help)"
        );
        assert_eq!(
            Bad::NeedsValue("count").words("network ping"),
            "network ping: --count needs a value"
        );
        assert_eq!(number("network ping", "count", "3"), Ok(3));
        assert_eq!(
            number("network ping", "count", "x").unwrap_err(),
            "network ping: --count wants a whole number (got x)"
        );
    }
}
