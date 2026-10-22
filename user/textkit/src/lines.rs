//! What `text` does to lines: its options, how `find` matches, how `sort` orders, runs of
//! equal lines for `unique`, line/word/byte counts for `count`, and the `[N]` of `first`
//! and `last`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Every `text` option; each verb takes some of them (see [`parse`]).
#[derive(Default, Debug, PartialEq)]
pub struct Opts {
    /// show, find: number the lines. sort: compare as numbers.
    pub numbers: bool,
    pub ignore_case: bool,
    pub invert: bool,
    /// find: how many lines matched. unique: how many times each line came.
    pub count: bool,
    /// find: only the names of the files with a match.
    pub names: bool,
    pub reverse: bool,
    pub unique: bool,
    /// unique: only lines that came more than once.
    pub repeated: bool,
    pub lines: bool,
    pub words: bool,
    pub bytes: bool,
    /// sort: compare from this whitespace-separated field on (1 is the first; 0 the line).
    pub field: usize,
}

/// The verbs `text` knows, in `text help` order.
pub const VERBS: [&str; 8] = [
    "show", "first", "last", "find", "sort", "unique", "count", "help",
];

fn takes(verb: &str, opt: &str) -> bool {
    let list: &[&str] = match verb {
        "show" => &["--numbers"],
        "find" => &[
            "--ignore-case",
            "--invert",
            "--numbers",
            "--count",
            "--names",
        ],
        "sort" => &["--reverse", "--numbers", "--unique", "--field"],
        "unique" => &["--count", "--repeated"],
        "count" => &["--lines", "--words", "--bytes"],
        _ => &[],
    };
    list.contains(&opt)
}

/// Options for `verb` out of `args`; the rest (pattern, files) in order. `--` ends the
/// options and `-` alone is stdin. Err: what to tell the user.
pub fn parse<'a>(verb: &str, args: &[&'a str]) -> Result<(Opts, Vec<&'a str>), String> {
    let mut o = Opts::default();
    let mut rest = Vec::new();
    let mut done = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        i += 1;
        if done || a == "-" || !a.starts_with('-') {
            rest.push(a);
            continue;
        }
        if a == "--" {
            done = true;
            continue;
        }
        let (name, value) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (a, None),
        };
        if !takes(verb, name) {
            return Err(format!(
                "text {verb}: there's no option {a} (text help lists them)"
            ));
        }
        match name {
            "--field" => {
                let v = match value {
                    Some(v) => v,
                    None if i < args.len() => {
                        i += 1;
                        args[i - 1]
                    }
                    None => {
                        return Err(String::from(
                            "text sort: --field needs a number (1 is the first field)",
                        ));
                    }
                };
                o.field = v.parse().map_err(|_| {
                    format!("text sort: --field needs a number (1 is the first field), not {v}")
                })?;
            }
            _ if value.is_some() => {
                return Err(format!("text {verb}: {name} doesn't take a value"));
            }
            "--numbers" => o.numbers = true,
            "--ignore-case" => o.ignore_case = true,
            "--invert" => o.invert = true,
            "--count" => o.count = true,
            "--names" => o.names = true,
            "--reverse" => o.reverse = true,
            "--unique" => o.unique = true,
            "--repeated" => o.repeated = true,
            "--lines" => o.lines = true,
            "--words" => o.words = true,
            _ => o.bytes = true, // --bytes, the last one takes() allows
        }
    }
    Ok((o, rest))
}

/// `first [N] FILE...` / `last [N] FILE...`: N (default 10) and the files. A file called
/// "5" is `./5`.
pub fn how_many<'a>(rest: &[&'a str]) -> (usize, Vec<&'a str>) {
    match rest.split_first() {
        Some((n, files)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            (n.parse().unwrap_or(usize::MAX), files.to_vec())
        }
        _ => (10, rest.to_vec()),
    }
}

/// `text find`: a plain piece of text (not a pattern language), maybe ignoring case.
pub struct Matcher {
    needle: String,
    ignore_case: bool,
    invert: bool,
}

impl Matcher {
    pub fn new(pat: &str, ignore_case: bool, invert: bool) -> Matcher {
        Matcher {
            needle: if ignore_case {
                pat.to_lowercase()
            } else {
                String::from(pat)
            },
            ignore_case,
            invert,
        }
    }

    /// Does `line` count (contains the text, or with --invert doesn't)?
    pub fn hit(&self, line: &str) -> bool {
        let found = if self.ignore_case {
            line.to_lowercase().contains(self.needle.as_str())
        } else {
            line.contains(self.needle.as_str())
        };
        found != self.invert
    }
}

/// The number at the start of `s` (leading blanks skipped; none: 0), for `sort --numbers`.
pub fn number(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && c == '-')))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse().unwrap_or(0.0)
}

/// `l` from whitespace-separated field `k` (1 is the first) to the end; 0: all of it.
pub fn from_field(l: &str, k: usize) -> &str {
    if k == 0 {
        return l;
    }
    let mut field = 0;
    let mut in_ws = true;
    for (i, c) in l.char_indices() {
        if !c.is_whitespace() && in_ws {
            field += 1;
            if field == k {
                return &l[i..];
            }
        }
        in_ws = c.is_whitespace();
    }
    ""
}

/// Sort the way `text sort` does: by text (or `numbers`), from `field` on, ties by the whole
/// line, then `reverse`, then `unique` drops repeats.
pub fn sort(lines: &mut Vec<String>, o: &Opts) {
    let k = o.field;
    if o.numbers {
        lines.sort_by(|a, b| {
            number(from_field(a, k))
                .partial_cmp(&number(from_field(b, k)))
                .unwrap_or(Ordering::Equal)
                .then(a.cmp(b))
        });
    } else {
        lines.sort_by(|a, b| from_field(a, k).cmp(from_field(b, k)).then(a.cmp(b)));
    }
    if o.reverse {
        lines.reverse();
    }
    if o.unique {
        lines.dedup();
    }
}

/// Runs of equal lines next to each other (`text unique`): push lines in, get each run back
/// (the line, how many) once it ends; `finish` gives the last.
#[derive(Default)]
pub struct Runs {
    prev: Option<(String, usize)>,
}

impl Runs {
    pub fn push(&mut self, l: String) -> Option<(String, usize)> {
        match &mut self.prev {
            Some((p, n)) if *p == l => {
                *n += 1;
                None
            }
            _ => self.prev.replace((l, 1)),
        }
    }

    pub fn finish(&mut self) -> Option<(String, usize)> {
        self.prev.take()
    }
}

/// Lines, words and bytes (`text count`): a line is a newline, a word a run of non-blanks.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Counts {
    pub lines: u64,
    pub words: u64,
    pub bytes: u64,
    in_word: bool,
}

impl Counts {
    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            self.bytes += 1;
            if b == b'\n' {
                self.lines += 1;
            }
            let space = b.is_ascii_whitespace();
            if !space && !self.in_word {
                self.words += 1;
            }
            self.in_word = !space;
        }
    }

    pub fn add(&mut self, o: &Counts) {
        self.lines += o.lines;
        self.words += o.words;
        self.bytes += o.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| String::from(*s)).collect()
    }

    #[test]
    fn options() {
        let (o, rest) = parse("find", &["--ignore-case", "msh", "a.txt", "--invert"]).unwrap();
        assert!(o.ignore_case && o.invert && !o.numbers);
        assert_eq!(rest, vec!["msh", "a.txt"]);
        let (o, rest) = parse("find", &["--", "--names", "-"]).unwrap();
        assert!(!o.names);
        assert_eq!(rest, vec!["--names", "-"]);
        let (o, _) = parse("sort", &["--field", "5", "--numbers", "--reverse"]).unwrap();
        assert_eq!((o.field, o.numbers, o.reverse), (5, true, true));
        assert_eq!(parse("sort", &["--field=2"]).unwrap().0.field, 2);
        let (o, _) = parse("count", &["--lines", "--bytes"]).unwrap();
        assert!(o.lines && o.bytes && !o.words);
        let (o, _) = parse("unique", &["--count", "--repeated"]).unwrap();
        assert!(o.count && o.repeated);
        assert!(parse("show", &["--numbers"]).unwrap().0.numbers);
    }

    #[test]
    fn option_errors() {
        assert_eq!(
            parse("show", &["--invert"]).unwrap_err(),
            "text show: there's no option --invert (text help lists them)"
        );
        assert_eq!(
            parse("find", &["-i", "x"]).unwrap_err(),
            "text find: there's no option -i (text help lists them)"
        );
        assert!(
            parse("sort", &["--field"])
                .unwrap_err()
                .contains("needs a number")
        );
        assert!(
            parse("sort", &["--field", "x"])
                .unwrap_err()
                .ends_with("not x")
        );
        assert_eq!(
            parse("count", &["--lines=3"]).unwrap_err(),
            "text count: --lines doesn't take a value"
        );
    }

    #[test]
    fn first_and_last() {
        assert_eq!(how_many(&["3", "a"]), (3, vec!["a"]));
        assert_eq!(how_many(&["a", "b"]), (10, vec!["a", "b"]));
        assert_eq!(how_many(&["0"]), (0, vec![]));
        assert_eq!(how_many(&[]), (10, vec![]));
        assert_eq!(how_many(&["./5"]), (10, vec!["./5"]));
    }

    #[test]
    fn matching() {
        let m = Matcher::new("msh", false, false);
        assert!(m.hit("  12 msh  sleeping"));
        assert!(!m.hit("MSH"));
        assert!(Matcher::new("msh", true, false).hit("MSH"));
        assert!(Matcher::new("MSH", true, false).hit("a msh b"));
        assert!(Matcher::new("msh", false, true).hit("init"));
        assert!(!Matcher::new("msh", false, true).hit("msh"));
        assert!(Matcher::new("", false, false).hit("anything"));
    }

    #[test]
    fn sorting() {
        let mut v = strings(&["b", "c", "a", "b"]);
        sort(&mut v, &Opts::default());
        assert_eq!(v, strings(&["a", "b", "b", "c"]));
        let mut v = strings(&["10", "9", "-1", "x", "2.5"]);
        sort(
            &mut v,
            &Opts {
                numbers: true,
                ..Opts::default()
            },
        );
        assert_eq!(v, strings(&["-1", "x", "2.5", "9", "10"]));
        let mut v = strings(&["a 3", "b 1", "c 2", "b 1"]);
        let o = Opts {
            numbers: true,
            field: 2,
            reverse: true,
            unique: true,
            ..Opts::default()
        };
        sort(&mut v, &o);
        assert_eq!(v, strings(&["a 3", "c 2", "b 1"]));
        assert_eq!(from_field("  one two  three", 2), "two  three");
        assert_eq!(from_field("one", 4), "");
        assert_eq!(number("  42kb"), 42.0);
    }

    #[test]
    fn runs() {
        let mut r = Runs::default();
        let mut got = Vec::new();
        for l in ["a", "a", "b", "a", "a", "a"] {
            got.extend(r.push(String::from(l)));
        }
        got.extend(r.finish());
        assert_eq!(
            got,
            vec![
                (String::from("a"), 2),
                (String::from("b"), 1),
                (String::from("a"), 3)
            ]
        );
        assert_eq!(Runs::default().finish(), None);
    }

    #[test]
    fn counting() {
        let mut c = Counts::default();
        c.feed(b"one two\nthr");
        c.feed(b"ee  four\n\n");
        assert_eq!((c.lines, c.words, c.bytes), (3, 4, 21));
        let mut t = Counts::default();
        t.add(&c);
        t.add(&c);
        assert_eq!((t.lines, t.words, t.bytes), (6, 8, 42));
        let mut e = Counts::default();
        e.feed(b"");
        assert_eq!((e.lines, e.words, e.bytes), (0, 0, 0));
    }
}
