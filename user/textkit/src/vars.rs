//! msh's `variables`: what a variable name is, `variables set NAME=value...` and
//! `variables run NAME=value... COMMAND ARGS`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Letters, digits and `_`, not starting with a digit (what msh's `NAME=value` takes).
pub fn valid_name(n: &str) -> bool {
    let mut b = n.bytes();
    matches!(b.next(), Some(c) if c == b'_' || c.is_ascii_alphabetic())
        && b.all(|c| c == b'_' || c.is_ascii_alphanumeric())
}

pub fn bad_name(n: &str) -> String {
    format!("{n} isn't a variable name (letters, digits and _, not starting with a digit)")
}

/// `NAME=value` split in two, if NAME is a variable name.
pub fn assignment(w: &str) -> Option<(&str, &str)> {
    w.split_once('=').filter(|(k, _)| valid_name(k))
}

/// `variables set` words: `NAME=value`, or a bare `NAME` (export the one the shell has).
pub fn parse_set<S: AsRef<str>>(args: &[S]) -> Result<Vec<(String, Option<String>)>, String> {
    if args.is_empty() {
        return Err(String::from(
            "variables set: NAME=value needed (variables set NAME=value...)",
        ));
    }
    args.iter()
        .map(|a| {
            let a = a.as_ref();
            let (k, v) = match a.split_once('=') {
                Some((k, v)) => (k, Some(String::from(v))),
                None => (a, None),
            };
            if valid_name(k) {
                Ok((String::from(k), v))
            } else {
                Err(format!("variables set: {}", bad_name(k)))
            }
        })
        .collect()
}

/// What `variables run` runs: the `NAME=value`s, then the command and its arguments.
pub type Run = (Vec<(String, String)>, Vec<String>);

/// `variables run` words: the leading `NAME=value`s, then the command and its arguments.
pub fn split_run<S: AsRef<str>>(args: &[S]) -> Result<Run, String> {
    let mut assigns = Vec::new();
    let mut i = 0;
    while let Some((k, v)) = args.get(i).and_then(|a| assignment(a.as_ref())) {
        assigns.push((String::from(k), String::from(v)));
        i += 1;
    }
    if i == args.len() {
        return Err(String::from(
            "variables run: a command is needed (variables run NAME=value... COMMAND)",
        ));
    }
    let cmd = args[i..].iter().map(|a| String::from(a.as_ref())).collect();
    Ok((assigns, cmd))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn names() {
        for ok in ["A", "_x", "PATH", "a1_b"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "1a", "A-B", "a b", "é"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(assignment("A=b=c"), Some(("A", "b=c")));
        assert_eq!(assignment("A="), Some(("A", "")));
        assert_eq!(assignment("1=x"), None);
        assert_eq!(assignment("plain"), None);
    }

    #[test]
    fn set() {
        assert_eq!(
            parse_set(&["A=1", "B"]).unwrap(),
            vec![
                (String::from("A"), Some(String::from("1"))),
                (String::from("B"), None)
            ]
        );
        assert_eq!(
            parse_set(&["A-B=1"]).unwrap_err(),
            "variables set: A-B isn't a variable name (letters, digits and _, not starting with a digit)"
        );
        assert!(
            parse_set::<&str>(&[])
                .unwrap_err()
                .contains("NAME=value needed")
        );
    }

    #[test]
    fn run() {
        let (a, c) = split_run(&["A=1", "B=two words", "printenv", "C=3"]).unwrap();
        assert_eq!(
            a,
            vec![
                (String::from("A"), String::from("1")),
                (String::from("B"), String::from("two words"))
            ]
        );
        assert_eq!(c, vec![String::from("printenv"), String::from("C=3")]);
        let (a, c) = split_run(&["ps"]).unwrap();
        assert!(a.is_empty());
        assert_eq!(c, vec![String::from("ps")]);
        assert!(
            split_run(&["A=1"])
                .unwrap_err()
                .contains("a command is needed")
        );
        assert!(split_run::<&str>(&[]).is_err());
    }
}
