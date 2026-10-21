//! Tokens. Words keep their quoting so expansion can happen at run time (`a=1; echo $a`).

use alloc::string::String;
use alloc::vec::Vec;

/// One piece of a word.
#[derive(Clone, Debug)]
pub enum Part {
    /// Literal text; `quoted` = from '...' or "..." or a backslash escape (no globbing).
    Lit(String, bool),
    /// `$NAME`, `${NAME}`, `$?`, `$$`, `$0`... (expansions are never re-split or globbed).
    Var(String),
    /// `~` at the start of a word.
    Home,
}

pub type Word = Vec<Part>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RedirKind {
    /// `<`
    In,
    /// `>`
    Out,
    /// `>>`
    Append,
    /// `n>&m`: the target word is an fd number.
    Dup,
}

#[derive(Clone, Debug)]
pub enum Tok {
    Word(Word),
    Pipe,
    OrIf,
    AndIf,
    Semi,
    Amp,
    /// fd (default 0 for `<`, 1 for the others), kind; the target word follows.
    Redir(i32, RedirKind),
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[allow(unused_assignments)] // the end_word! macro resets state the last call doesn't read
pub fn lex(line: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = line.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let mut word: Word = Vec::new();
    let mut lit = String::new();
    let mut lit_quoted = false;
    let mut in_word = false;

    macro_rules! flush_lit {
        () => {
            if !lit.is_empty() {
                word.push(Part::Lit(core::mem::take(&mut lit), lit_quoted));
            }
        };
    }
    macro_rules! end_word {
        () => {
            flush_lit!();
            if in_word {
                toks.push(Tok::Word(core::mem::take(&mut word)));
                in_word = false;
            }
        };
    }
    // Start a literal run with the given quotedness.
    macro_rules! lit_mode {
        ($q:expr) => {
            if lit_quoted != $q {
                flush_lit!();
                lit_quoted = $q;
            }
        };
    }

    // `$...` at chars[i] == '$'. Returns the new index.
    let var = |chars: &[char], mut i: usize, word: &mut Word| -> usize {
        i += 1;
        match chars.get(i) {
            Some('{') => {
                let start = i + 1;
                let end = chars[start..]
                    .iter()
                    .position(|&c| c == '}')
                    .map(|p| start + p);
                match end {
                    Some(e) => {
                        word.push(Part::Var(chars[start..e].iter().collect()));
                        e + 1
                    }
                    None => {
                        word.push(Part::Lit(String::from("${"), true));
                        start
                    }
                }
            }
            Some(&c) if c == '?' || c == '$' || c == '#' || c.is_ascii_digit() => {
                word.push(Part::Var(String::from(c)));
                i + 1
            }
            Some(&c) if is_name_char(c) => {
                let start = i;
                while chars.get(i).is_some_and(|&c| is_name_char(c)) {
                    i += 1;
                }
                word.push(Part::Var(chars[start..i].iter().collect()));
                i
            }
            _ => {
                word.push(Part::Lit(String::from("$"), true));
                i
            }
        }
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                end_word!();
                i += 1;
            }
            '#' if !in_word => break,
            '\'' => {
                in_word = true;
                lit_mode!(true);
                let start = i + 1;
                let Some(end) = chars[start..].iter().position(|&c| c == '\'') else {
                    return Err(String::from("unterminated '"));
                };
                lit.extend(&chars[start..start + end]);
                if end == 0 {
                    // '' is an empty word, not nothing.
                    word.push(Part::Lit(String::new(), true));
                }
                i = start + end + 1;
            }
            '"' => {
                in_word = true;
                i += 1;
                let mut empty = true;
                loop {
                    match chars.get(i) {
                        None => return Err(String::from("unterminated \"")),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') if matches!(chars.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            lit_mode!(true);
                            lit.push(chars[i + 1]);
                            i += 2;
                        }
                        Some('$') => {
                            flush_lit!();
                            i = var(&chars, i, &mut word);
                        }
                        Some(&ch) => {
                            lit_mode!(true);
                            lit.push(ch);
                            i += 1;
                        }
                    }
                    empty = false;
                }
                if empty {
                    word.push(Part::Lit(String::new(), true));
                }
            }
            '\\' => {
                in_word = true;
                if let Some(&n) = chars.get(i + 1) {
                    lit_mode!(true);
                    lit.push(n);
                }
                i += 2;
            }
            '$' => {
                in_word = true;
                flush_lit!();
                i = var(&chars, i, &mut word);
            }
            '~' if !in_word
                && matches!(
                    chars.get(i + 1),
                    None | Some('/' | ' ' | '\t' | ';' | '|' | '&' | '>' | '<')
                ) =>
            {
                in_word = true;
                word.push(Part::Home);
                i += 1;
            }
            '|' => {
                end_word!();
                if chars.get(i + 1) == Some(&'|') {
                    toks.push(Tok::OrIf);
                    i += 2;
                } else {
                    toks.push(Tok::Pipe);
                    i += 1;
                }
            }
            '&' => {
                end_word!();
                if chars.get(i + 1) == Some(&'&') {
                    toks.push(Tok::AndIf);
                    i += 2;
                } else {
                    toks.push(Tok::Amp);
                    i += 1;
                }
            }
            ';' => {
                end_word!();
                toks.push(Tok::Semi);
                i += 1;
            }
            '<' | '>' => {
                // A word made only of digits right before the operator is its fd: 2>err
                flush_lit!();
                let mut fd = if c == '<' { 0 } else { 1 };
                if in_word
                    && word.len() == 1
                    && let Part::Lit(s, false) = &word[0]
                    && !s.is_empty()
                    && s.chars().all(|c| c.is_ascii_digit())
                {
                    fd = s.parse().unwrap_or(fd);
                    word.clear();
                    in_word = false;
                }
                end_word!();
                let kind = if c == '<' {
                    i += 1;
                    RedirKind::In
                } else if chars.get(i + 1) == Some(&'>') {
                    i += 2;
                    RedirKind::Append
                } else if chars.get(i + 1) == Some(&'&') {
                    i += 2;
                    RedirKind::Dup
                } else {
                    i += 1;
                    RedirKind::Out
                };
                toks.push(Tok::Redir(fd, kind));
            }
            _ => {
                in_word = true;
                lit_mode!(false);
                lit.push(c);
                i += 1;
            }
        }
    }
    end_word!();
    Ok(toks)
}
