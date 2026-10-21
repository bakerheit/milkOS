//! Grammar (a small POSIX-sh subset):
//! ```text
//! list     := and_or (( ';' | '&' ) and_or)* [ ';' | '&' ]
//! and_or   := pipeline (( '&&' | '||' ) pipeline)*
//! pipeline := command ( '|' command )*
//! command  := ( NAME=value )* ( word | redirection )*
//! redirection := [n] ( '<' | '>' | '>>' ) word  |  [n] '>&' m
//! ```

use alloc::string::String;
use alloc::vec::Vec;

use crate::lex::{Part, RedirKind, Tok, Word};

#[derive(Clone, Debug)]
pub struct Redir {
    pub fd: i32,
    pub kind: RedirKind,
    pub target: Word,
}

#[derive(Clone, Debug, Default)]
pub struct Command {
    /// Leading `NAME=value` words.
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    pub redirs: Vec<Redir>,
}

pub type Pipeline = Vec<Command>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Join {
    And,
    Or,
}

#[derive(Clone, Debug)]
pub struct AndOr {
    pub first: Pipeline,
    pub rest: Vec<(Join, Pipeline)>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub job: AndOr,
    pub background: bool,
}

/// If `w` is `NAME=...` (NAME unquoted, valid), split it.
fn assignment(w: &Word) -> Option<(String, Word)> {
    let Some(Part::Lit(first, false)) = w.first() else {
        return None;
    };
    let eq = first.find('=')?;
    let name = &first[..eq];
    if name.is_empty()
        || name.starts_with(|c: char| c.is_ascii_digit())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut value: Word = Vec::new();
    let rest = &first[eq + 1..];
    if !rest.is_empty() {
        value.push(Part::Lit(String::from(rest), false));
    }
    value.extend(w[1..].iter().cloned());
    Some((String::from(name), value))
}

/// `where COL` so far, with no operator yet: the next `<` / `>` is the comparison.
/// (Later ones, as in `where size > 1M > out`, are ordinary redirections.)
fn is_where(c: &Command) -> bool {
    matches!(c.words.first().and_then(|w| w.first()), Some(Part::Lit(s, false)) if s == "where")
        && c.words.first().is_some_and(|w| w.len() == 1)
        && c.words.len() == 2
}

pub fn parse(toks: Vec<Tok>) -> Result<Vec<Item>, String> {
    let mut items = Vec::new();
    let mut it = toks.into_iter().peekable();
    let mut cur_cmd = Command::default();
    let mut pipeline: Pipeline = Vec::new();
    let mut andor: Option<AndOr> = None;
    let mut pending_join: Option<Join> = None;

    fn finish_cmd(cmd: &mut Command, pipeline: &mut Pipeline) -> Result<(), String> {
        if cmd.words.is_empty() && cmd.assigns.is_empty() && cmd.redirs.is_empty() {
            return Err(String::from("syntax error: empty command"));
        }
        pipeline.push(core::mem::take(cmd));
        Ok(())
    }
    fn finish_pipeline(
        pipeline: &mut Pipeline,
        andor: &mut Option<AndOr>,
        join: &mut Option<Join>,
    ) {
        let p = core::mem::take(pipeline);
        match (andor.as_mut(), join.take()) {
            (Some(a), Some(j)) => a.rest.push((j, p)),
            _ => {
                *andor = Some(AndOr {
                    first: p,
                    rest: Vec::new(),
                })
            }
        }
    }

    loop {
        let tok = it.next();
        match tok {
            Some(Tok::Word(w)) => {
                if cur_cmd.words.is_empty()
                    && let Some(a) = assignment(&w)
                {
                    cur_cmd.assigns.push(a);
                } else {
                    cur_cmd.words.push(w);
                }
            }
            // In `where COL > 100` the comparison operators are words, not redirections.
            Some(Tok::Redir(fd, kind)) if is_where(&cur_cmd) => {
                let op = match kind {
                    RedirKind::In => "<",
                    RedirKind::Out => ">",
                    RedirKind::Append => ">>",
                    RedirKind::Dup => ">&",
                };
                let default_fd = if kind == RedirKind::In { 0 } else { 1 };
                let text = if fd == default_fd {
                    String::from(op)
                } else {
                    alloc::format!("{fd}{op}")
                };
                cur_cmd.words.push(alloc::vec![Part::Lit(text, true)]);
            }
            Some(Tok::Redir(fd, kind)) => match it.next() {
                Some(Tok::Word(target)) => cur_cmd.redirs.push(Redir { fd, kind, target }),
                _ => return Err(String::from("syntax error: redirection needs a target")),
            },
            Some(Tok::Pipe) => finish_cmd(&mut cur_cmd, &mut pipeline)?,
            Some(Tok::AndIf) | Some(Tok::OrIf) => {
                finish_cmd(&mut cur_cmd, &mut pipeline)?;
                finish_pipeline(&mut pipeline, &mut andor, &mut pending_join);
                pending_join = Some(if matches!(tok, Some(Tok::AndIf)) {
                    Join::And
                } else {
                    Join::Or
                });
            }
            Some(Tok::Semi) | Some(Tok::Amp) | None => {
                let empty = cur_cmd.words.is_empty()
                    && cur_cmd.assigns.is_empty()
                    && cur_cmd.redirs.is_empty()
                    && pipeline.is_empty();
                if empty {
                    if pending_join.is_some() {
                        return Err(String::from(
                            "syntax error: && or || needs a command after it",
                        ));
                    }
                    if andor.is_none() {
                        if tok.is_none() {
                            break;
                        }
                        if matches!(tok, Some(Tok::Amp)) {
                            return Err(String::from("syntax error near &"));
                        }
                        continue; // stray ;
                    }
                } else {
                    finish_cmd(&mut cur_cmd, &mut pipeline)?;
                    finish_pipeline(&mut pipeline, &mut andor, &mut pending_join);
                }
                if let Some(job) = andor.take() {
                    items.push(Item {
                        job,
                        background: matches!(tok, Some(Tok::Amp)),
                    });
                }
                if tok.is_none() {
                    break;
                }
            }
        }
    }
    Ok(items)
}
