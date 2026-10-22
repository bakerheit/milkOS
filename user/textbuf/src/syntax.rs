//! A small syntax pass for colouring: five roles (so any theme works) for Rust, C, shell and
//! Markdown, chosen by file extension. It works line by line and carries a little state from one
//! line to the next (a block comment, a code fence). It never fails: unknown text is just
//! [`Role::Text`].

use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Plain,
    Rust,
    C,
    Shell,
    Markdown,
}

impl Lang {
    /// From a file name (by its extension; a few special names like "Makefile" count as shell).
    pub fn from_file_name(name: &str) -> Lang {
        let lower = name.to_ascii_lowercase();
        match lower.rsplit_once('.').map(|(_, e)| e) {
            Some("rs") => Lang::Rust,
            Some("c" | "h" | "cc" | "cpp" | "hpp" | "cxx") => Lang::C,
            Some("sh" | "msh" | "bash" | "zsh") => Lang::Shell,
            Some("md" | "markdown") => Lang::Markdown,
            _ if lower == "makefile" => Lang::Shell,
            _ => Lang::Plain,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Lang::Plain => "Plain Text",
            Lang::Rust => "Rust",
            Lang::C => "C",
            Lang::Shell => "Shell",
            Lang::Markdown => "Markdown",
        }
    }

    /// Wrap lines by default? On for prose, off for code.
    pub fn wraps_by_default(&self) -> bool {
        matches!(self, Lang::Plain | Lang::Markdown)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Text,
    Keyword,
    String,
    Number,
    Comment,
    /// Markdown: a heading line (drawn bold).
    Heading,
    /// Markdown: a list marker.
    Marker,
    /// Markdown: inline code, or a fenced block.
    Code,
    /// Markdown: bold text between `**`.
    Strong,
}

/// What carries from one line to the next.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct State {
    /// Inside a `/* ... */` comment.
    pub block_comment: bool,
    /// Inside a Markdown ``` fence.
    pub fence: bool,
}

/// A coloured piece of a line: chars `start..end` have role `role`. Pieces don't overlap and are in
/// order; gaps are [`Role::Text`].
pub type Span = (usize, usize, Role);

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const C_KW: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "bool",
    "true",
    "false",
    "NULL",
    "class",
    "namespace",
    "template",
    "public",
    "private",
    "new",
    "delete",
];
const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "return", "exit", "export", "local", "let", "set", "unset", "source",
    "alias",
];

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Colour one line of `lang`. `st` is the state at the start of the line and is updated to the
/// state at its end.
pub fn highlight(lang: Lang, line: &str, st: &mut State) -> Vec<Span> {
    match lang {
        Lang::Plain => Vec::new(),
        Lang::Markdown => markdown(line, st),
        _ => code(lang, line, st),
    }
}

fn code(lang: Lang, line: &str, st: &mut State) -> Vec<Span> {
    let cs: Vec<char> = line.chars().collect();
    let kw = match lang {
        Lang::Rust => RUST_KW,
        Lang::C => C_KW,
        _ => SH_KW,
    };
    let slashes = lang != Lang::Shell;
    let mut out: Vec<Span> = Vec::new();
    let mut i = 0;
    let n = cs.len();
    while i < n {
        if st.block_comment {
            let mut j = i;
            while j + 1 < n && !(cs[j] == '*' && cs[j + 1] == '/') {
                j += 1;
            }
            if j + 1 < n {
                out.push((i, j + 2, Role::Comment));
                st.block_comment = false;
                i = j + 2;
            } else {
                out.push((i, n, Role::Comment));
                i = n;
            }
            continue;
        }
        let c = cs[i];
        if (slashes && c == '/' && cs.get(i + 1) == Some(&'/')) || (!slashes && c == '#') {
            out.push((i, n, Role::Comment));
            break;
        }
        if slashes && c == '/' && cs.get(i + 1) == Some(&'*') {
            st.block_comment = true;
            continue_block(&cs, &mut i, &mut out, st);
            continue;
        }
        if lang == Lang::C && c == '#' && cs[..i].iter().all(|x| x.is_whitespace()) {
            // A preprocessor line.
            out.push((i, n, Role::Keyword));
            break;
        }
        if c == '"'
            || (c == '\'' && !(lang == Lang::Rust && is_lifetime(&cs, i)))
            || (lang == Lang::Shell && c == '`')
        {
            let q = c;
            let mut j = i + 1;
            while j < n && cs[j] != q {
                // A backslash escapes the next char (but not inside shell single quotes).
                if cs[j] == '\\' && !(lang == Lang::Shell && q == '\'') {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            out.push((i, end, Role::String));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !is_ident(cs[i - 1])) {
            let mut j = i;
            while j < n && (cs[j].is_ascii_alphanumeric() || cs[j] == '_' || cs[j] == '.') {
                j += 1;
            }
            out.push((i, j, Role::Number));
            i = j;
            continue;
        }
        if is_ident(c) {
            let mut j = i;
            while j < n && is_ident(cs[j]) {
                j += 1;
            }
            let word: alloc::string::String = cs[i..j].iter().collect();
            if kw.contains(&word.as_str()) {
                out.push((i, j, Role::Keyword));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// At a `/*`: colour to its end on this line (or the whole rest) and move past it.
fn continue_block(cs: &[char], i: &mut usize, out: &mut Vec<Span>, st: &mut State) {
    let n = cs.len();
    let mut j = *i + 2;
    while j + 1 < n && !(cs[j] == '*' && cs[j + 1] == '/') {
        j += 1;
    }
    if j + 1 < n {
        out.push((*i, j + 2, Role::Comment));
        st.block_comment = false;
        *i = j + 2;
    } else {
        out.push((*i, n, Role::Comment));
        *i = n;
    }
}

/// Rust `'a` is a lifetime, `'a'` a char: it is a lifetime when no closing quote follows within
/// a short way.
fn is_lifetime(cs: &[char], i: usize) -> bool {
    let next = cs.get(i + 1).copied().unwrap_or(' ');
    if !(next.is_alphabetic() || next == '_') {
        return false;
    }
    cs.get(i + 2) != Some(&'\'')
}

fn markdown(line: &str, st: &mut State) -> Vec<Span> {
    let cs: Vec<char> = line.chars().collect();
    let n = cs.len();
    let t = line.trim_start();
    if t.starts_with("```") || t.starts_with("~~~") {
        st.fence = !st.fence;
        return alloc::vec![(0, n, Role::Code)];
    }
    if st.fence {
        return alloc::vec![(0, n, Role::Code)];
    }
    if t.starts_with('#') {
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if hashes <= 6 && t.chars().nth(hashes).is_none_or(|c| c == ' ') {
            return alloc::vec![(0, n, Role::Heading)];
        }
    }
    let mut out: Vec<Span> = Vec::new();
    let lead = cs.iter().take_while(|c| c.is_whitespace()).count();
    // List markers: "- ", "* ", "+ ", "1. ".
    let rest: alloc::string::String = cs[lead..].iter().collect();
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    let marker = if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        Some(2)
    } else if digits > 0 && rest[digits..].starts_with(". ") {
        Some(digits + 2)
    } else {
        None
    };
    if let Some(m) = marker {
        out.push((lead, lead + m, Role::Marker));
    }
    let mut i = lead + marker.unwrap_or(0);
    while i < n {
        let end = if cs[i] == '`' {
            (i + 1..n)
                .find(|&j| cs[j] == '`')
                .map(|j| (j + 1, Role::Code))
        } else if cs[i] == '*' && cs.get(i + 1) == Some(&'*') {
            (i + 2..n.saturating_sub(1))
                .find(|&j| cs[j] == '*' && cs[j + 1] == '*')
                .map(|j| (j + 2, Role::Strong))
        } else {
            None
        };
        match end {
            Some((e, role)) => {
                out.push((i, e, role));
                i = e;
            }
            None => i += 1,
        }
    }
    out.sort_by_key(|s| s.0);
    out
}

/// The state at the start of every line of `lines` (index = line number).
pub fn states<'a>(lang: Lang, lines: impl Iterator<Item = &'a str>) -> Vec<State> {
    let mut st = State::default();
    let mut out = Vec::new();
    for l in lines {
        out.push(st);
        let _ = highlight(lang, l, &mut st);
    }
    out
}
