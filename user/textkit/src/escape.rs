//! `print --escapes`: the backslash sequences echo -e understood, and nothing more.
//!
//! `\n` newline, `\t` tab, `\e` escape (for terminal colours), `\\` a backslash, `\c` stop
//! here with no newline. Any other `\x` stays as typed.

use alloc::string::String;

/// Append `w` to `out` with its escapes worked out. False: `\c` was seen (stop printing,
/// no newline at the end).
pub fn expand(w: &str, out: &mut String) -> bool {
    let mut it = w.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('c') => return false,
            Some(o) => {
                out.push('\\');
                out.push(o)
            }
            None => out.push('\\'),
        }
    }
    true
}

/// What `print` writes for `words`: joined with spaces, escapes if asked, a newline unless
/// `no_newline` (or a `\c`).
pub fn line(words: &[&str], escapes: bool, no_newline: bool) -> String {
    let mut s = String::new();
    let mut newline = !no_newline;
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        if !escapes {
            s.push_str(w);
        } else if !expand(w, &mut s) {
            newline = false;
            break;
        }
    }
    if newline {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain() {
        assert_eq!(line(&["hello", "world"], false, false), "hello world\n");
        assert_eq!(line(&["a\\nb"], false, false), "a\\nb\n");
        assert_eq!(line(&["x"], false, true), "x");
        assert_eq!(line(&[], false, false), "\n");
    }

    #[test]
    fn escapes() {
        assert_eq!(line(&["a\\nb\\tc"], true, false), "a\nb\tc\n");
        assert_eq!(line(&["\\e[31mred"], true, false), "\x1b[31mred\n");
        assert_eq!(line(&["back\\\\slash"], true, false), "back\\slash\n");
        assert_eq!(line(&["\\q"], true, false), "\\q\n");
        assert_eq!(line(&["end\\"], true, false), "end\\\n");
        assert_eq!(line(&["stop\\chere", "more"], true, false), "stop");
    }
}
