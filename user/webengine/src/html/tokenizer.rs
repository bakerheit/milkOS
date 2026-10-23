//! The HTML tokenizer, after WHATWG HTML §13.2.5 "Tokenization".
//!
//! It works on the bytes of a UTF-8 string whose newlines are already normalized (CR LF and CR
//! become LF, §13.2.3.5). Every state transition happens on an ASCII byte, so the bytes of a
//! multi-byte character always land in the same buffer and the buffers stay valid UTF-8.
//!
//! Simplifications, none of which change the tree for real pages:
//! - parse errors are not reported (the spec's recovery is followed, silently);
//! - the comment "less-than sign" states (§13.2.5.46-49) only exist to report errors, so they
//!   are left out: the comment text comes out the same;
//! - script data uses the RAWTEXT-style end-tag states without the "escaped" states for
//!   `<!--` inside scripts (§13.2.5.20-31): scripts are never run, only skipped;
//! - a NUL in the data state is dropped (the tree builder would ignore it in every mode that
//!   matters for display).

use alloc::string::String;
use alloc::vec::Vec;

use super::entities::{ENTITIES, MAX_LEN};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagAttr {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Doctype {
        name: Option<String>,
        public_id: Option<String>,
        system_id: Option<String>,
        force_quirks: bool,
    },
    StartTag {
        name: String,
        attrs: Vec<TagAttr>,
        self_closing: bool,
    },
    EndTag {
        name: String,
    },
    /// A run of characters (never empty).
    Chars(String),
    Comment(String),
    Eof,
}

/// The states the tree builder switches the tokenizer into (§13.2.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextState {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    /// `<` seen in RCDATA / RAWTEXT / script data (the kind is in `raw_kind`).
    RawLessThan,
    RawEndTagOpen,
    RawEndTagName,
    BeforeAttrName,
    AttrName,
    AfterAttrName,
    BeforeAttrValue,
    AttrValueDq,
    AttrValueSq,
    AttrValueUnq,
    AfterAttrValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    BeforeDoctypeId,
    DoctypeIdDq,
    DoctypeIdSq,
    AfterDoctypeId,
    BogusDoctype,
    CdataSection,
}

pub struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
    state: State,
    /// Which text state RawLessThan & co. return to.
    raw_kind: State,
    /// The pending character run.
    text: Vec<u8>,
    /// A finished token waiting behind `text`.
    pending: Option<Token>,
    // The tag being built.
    tag_name: Vec<u8>,
    tag_end: bool,
    self_closing: bool,
    attrs: Vec<TagAttr>,
    attr_name: Vec<u8>,
    attr_value: Vec<u8>,
    attr_open: bool,
    // Comment / doctype being built.
    comment: Vec<u8>,
    dt_name: Option<Vec<u8>>,
    dt_public: Option<Vec<u8>>,
    dt_system: Option<Vec<u8>>,
    dt_quirks: bool,
    /// Which id (public = true) the doctype id states fill.
    dt_public_next: bool,
    /// The temporary buffer of the raw end-tag states.
    temp: Vec<u8>,
    /// The name of the last start tag emitted (for "appropriate end tag").
    last_start: Vec<u8>,
    /// Set by the tree builder: the adjusted current node is foreign (CDATA allowed).
    pub allow_cdata: bool,
    eof_sent: bool,
}

fn is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0c | b' ' | b'\r')
}

fn to_string(v: Vec<u8>) -> String {
    match String::from_utf8(v) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

fn push_char(v: &mut Vec<u8>, c: char) {
    let mut b = [0u8; 4];
    v.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
}

const REPLACEMENT: char = '\u{FFFD}';

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Tokenizer<'a> {
        Tokenizer {
            input: input.as_bytes(),
            pos: 0,
            state: State::Data,
            raw_kind: State::Rawtext,
            text: Vec::new(),
            pending: None,
            tag_name: Vec::new(),
            tag_end: false,
            self_closing: false,
            attrs: Vec::new(),
            attr_name: Vec::new(),
            attr_value: Vec::new(),
            attr_open: false,
            comment: Vec::new(),
            dt_name: None,
            dt_public: None,
            dt_system: None,
            dt_quirks: false,
            dt_public_next: true,
            temp: Vec::new(),
            last_start: Vec::new(),
            allow_cdata: false,
            eof_sent: false,
        }
    }

    /// Switch to one of the text states (the tree builder does this after `<title>`,
    /// `<style>`, `<script>`, `<plaintext>` and the like).
    pub fn set_state(&mut self, s: TextState) {
        self.state = match s {
            TextState::Data => State::Data,
            TextState::Rcdata => State::Rcdata,
            TextState::Rawtext => State::Rawtext,
            TextState::ScriptData => State::ScriptData,
            TextState::Plaintext => State::Plaintext,
        };
    }

    /// Bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn starts_with_ci(&self, s: &[u8]) -> bool {
        let end = self.pos + s.len();
        end <= self.input.len() && self.input[self.pos..end].eq_ignore_ascii_case(s)
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.input[self.pos..].starts_with(s)
    }

    /// Queue a finished non-character token.
    fn emit(&mut self, t: Token) {
        debug_assert!(self.pending.is_none());
        self.pending = Some(t);
    }

    fn start_tag(&mut self, end: bool) {
        self.tag_name.clear();
        self.tag_end = end;
        self.self_closing = false;
        self.attrs.clear();
        self.attr_open = false;
    }

    fn start_attr(&mut self) {
        self.finish_attr();
        self.attr_name.clear();
        self.attr_value.clear();
        self.attr_open = true;
    }

    fn finish_attr(&mut self) {
        if !self.attr_open {
            return;
        }
        self.attr_open = false;
        let name = to_string(core::mem::take(&mut self.attr_name));
        // §13.2.5.33: a duplicate attribute is dropped (the first one wins).
        if self.attrs.iter().any(|a| a.name == name) {
            self.attr_value.clear();
            return;
        }
        let value = to_string(core::mem::take(&mut self.attr_value));
        self.attrs.push(TagAttr { name, value });
    }

    fn emit_tag(&mut self) {
        self.finish_attr();
        let name = to_string(core::mem::take(&mut self.tag_name));
        let t = if self.tag_end {
            Token::EndTag { name }
        } else {
            self.last_start.clear();
            self.last_start.extend_from_slice(name.as_bytes());
            Token::StartTag {
                name,
                attrs: core::mem::take(&mut self.attrs),
                self_closing: self.self_closing,
            }
        };
        self.emit(t);
    }

    fn emit_comment(&mut self) {
        let c = to_string(core::mem::take(&mut self.comment));
        self.emit(Token::Comment(c));
    }

    fn emit_doctype(&mut self) {
        let t = Token::Doctype {
            name: self.dt_name.take().map(to_string),
            public_id: self.dt_public.take().map(to_string),
            system_id: self.dt_system.take().map(to_string),
            force_quirks: self.dt_quirks,
        };
        self.emit(t);
    }

    fn new_doctype(&mut self) {
        self.dt_name = None;
        self.dt_public = None;
        self.dt_system = None;
        self.dt_quirks = false;
    }

    fn appropriate_end_tag(&self) -> bool {
        !self.last_start.is_empty() && self.tag_name == self.last_start
    }

    /// The next token. After [`Token::Eof`] it keeps returning Eof.
    pub fn next_token(&mut self) -> Token {
        loop {
            if let Some(t) = self.pending.take() {
                if !self.text.is_empty() {
                    self.pending = Some(t);
                    return Token::Chars(to_string(core::mem::take(&mut self.text)));
                }
                return t;
            }
            if self.eof_sent {
                return Token::Eof;
            }
            self.step();
        }
    }

    /// Run the state machine until a token is queued (or the input ends).
    fn step(&mut self) {
        while self.pending.is_none() {
            let Some(b) = self.peek() else {
                self.at_eof();
                return;
            };
            self.pos += 1;
            match self.state {
                State::Data => match b {
                    b'&' => self.char_ref(false),
                    b'<' => self.state = State::TagOpen,
                    0 => {} // dropped (see the module comment)
                    _ => {
                        // Fast path: copy the run up to the next special byte.
                        let start = self.pos - 1;
                        let mut end = self.pos;
                        while end < self.input.len() && !matches!(self.input[end], b'&' | b'<' | 0)
                        {
                            end += 1;
                        }
                        self.text.extend_from_slice(&self.input[start..end]);
                        self.pos = end;
                    }
                },
                State::Rcdata | State::Rawtext | State::ScriptData => match b {
                    b'&' if self.state == State::Rcdata => self.char_ref(false),
                    b'<' => {
                        self.raw_kind = self.state;
                        self.state = State::RawLessThan;
                    }
                    0 => push_char(&mut self.text, REPLACEMENT),
                    _ => {
                        let start = self.pos - 1;
                        let mut end = self.pos;
                        while end < self.input.len() && !matches!(self.input[end], b'&' | b'<' | 0)
                        {
                            end += 1;
                        }
                        self.text.extend_from_slice(&self.input[start..end]);
                        self.pos = end;
                    }
                },
                State::Plaintext => {
                    if b == 0 {
                        push_char(&mut self.text, REPLACEMENT);
                    } else {
                        self.text.push(b);
                    }
                }
                State::TagOpen => match b {
                    b'!' => self.state = State::MarkupDeclarationOpen,
                    b'/' => self.state = State::EndTagOpen,
                    b'a'..=b'z' | b'A'..=b'Z' => {
                        self.start_tag(false);
                        self.pos -= 1;
                        self.state = State::TagName;
                    }
                    b'?' => {
                        self.comment.clear();
                        self.pos -= 1;
                        self.state = State::BogusComment;
                    }
                    _ => {
                        self.text.push(b'<');
                        self.pos -= 1;
                        self.state = State::Data;
                    }
                },
                State::EndTagOpen => match b {
                    b'a'..=b'z' | b'A'..=b'Z' => {
                        self.start_tag(true);
                        self.pos -= 1;
                        self.state = State::TagName;
                    }
                    b'>' => self.state = State::Data,
                    _ => {
                        self.comment.clear();
                        self.pos -= 1;
                        self.state = State::BogusComment;
                    }
                },
                State::TagName => match b {
                    b if is_space(b) => self.state = State::BeforeAttrName,
                    b'/' => self.state = State::SelfClosingStartTag,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    0 => push_char(&mut self.tag_name, REPLACEMENT),
                    _ => self.tag_name.push(b.to_ascii_lowercase()),
                },
                State::RawLessThan => {
                    if b == b'/' {
                        self.temp.clear();
                        self.state = State::RawEndTagOpen;
                    } else {
                        self.text.push(b'<');
                        self.pos -= 1;
                        self.state = self.raw_kind;
                    }
                }
                State::RawEndTagOpen => {
                    if b.is_ascii_alphabetic() {
                        self.start_tag(true);
                        self.pos -= 1;
                        self.state = State::RawEndTagName;
                    } else {
                        self.text.extend_from_slice(b"</");
                        self.pos -= 1;
                        self.state = self.raw_kind;
                    }
                }
                State::RawEndTagName => {
                    if (is_space(b) || b == b'/' || b == b'>') && self.appropriate_end_tag() {
                        match b {
                            b'/' => self.state = State::SelfClosingStartTag,
                            b'>' => {
                                self.state = State::Data;
                                self.emit_tag();
                            }
                            _ => self.state = State::BeforeAttrName,
                        }
                    } else if b.is_ascii_alphabetic() {
                        self.tag_name.push(b.to_ascii_lowercase());
                        self.temp.push(b);
                    } else {
                        self.text.extend_from_slice(b"</");
                        let t = core::mem::take(&mut self.temp);
                        self.text.extend_from_slice(&t);
                        self.pos -= 1;
                        self.state = self.raw_kind;
                    }
                }
                State::BeforeAttrName => match b {
                    b if is_space(b) => {}
                    b'/' | b'>' => {
                        self.pos -= 1;
                        self.state = State::AfterAttrName;
                    }
                    b'=' => {
                        self.start_attr();
                        self.attr_name.push(b);
                        self.state = State::AttrName;
                    }
                    _ => {
                        self.start_attr();
                        self.pos -= 1;
                        self.state = State::AttrName;
                    }
                },
                State::AttrName => match b {
                    b if is_space(b) || b == b'/' || b == b'>' => {
                        self.pos -= 1;
                        self.state = State::AfterAttrName;
                    }
                    b'=' => self.state = State::BeforeAttrValue,
                    0 => push_char(&mut self.attr_name, REPLACEMENT),
                    _ => self.attr_name.push(b.to_ascii_lowercase()),
                },
                State::AfterAttrName => match b {
                    b if is_space(b) => {}
                    b'/' => self.state = State::SelfClosingStartTag,
                    b'=' => self.state = State::BeforeAttrValue,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    _ => {
                        self.start_attr();
                        self.pos -= 1;
                        self.state = State::AttrName;
                    }
                },
                State::BeforeAttrValue => match b {
                    b if is_space(b) => {}
                    b'"' => self.state = State::AttrValueDq,
                    b'\'' => self.state = State::AttrValueSq,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    _ => {
                        self.pos -= 1;
                        self.state = State::AttrValueUnq;
                    }
                },
                State::AttrValueDq | State::AttrValueSq => {
                    let q = if self.state == State::AttrValueDq {
                        b'"'
                    } else {
                        b'\''
                    };
                    match b {
                        b if b == q => self.state = State::AfterAttrValueQuoted,
                        b'&' => self.char_ref(true),
                        0 => push_char(&mut self.attr_value, REPLACEMENT),
                        _ => self.attr_value.push(b),
                    }
                }
                State::AttrValueUnq => match b {
                    b if is_space(b) => self.state = State::BeforeAttrName,
                    b'&' => self.char_ref(true),
                    b'>' => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    0 => push_char(&mut self.attr_value, REPLACEMENT),
                    _ => self.attr_value.push(b),
                },
                State::AfterAttrValueQuoted => match b {
                    b if is_space(b) => self.state = State::BeforeAttrName,
                    b'/' => self.state = State::SelfClosingStartTag,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    _ => {
                        self.pos -= 1;
                        self.state = State::BeforeAttrName;
                    }
                },
                State::SelfClosingStartTag => match b {
                    b'>' => {
                        self.self_closing = true;
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    _ => {
                        self.pos -= 1;
                        self.state = State::BeforeAttrName;
                    }
                },
                State::BogusComment => match b {
                    b'>' => {
                        self.state = State::Data;
                        self.emit_comment();
                    }
                    0 => push_char(&mut self.comment, REPLACEMENT),
                    _ => self.comment.push(b),
                },
                State::MarkupDeclarationOpen => {
                    self.pos -= 1;
                    if self.starts_with(b"--") {
                        self.pos += 2;
                        self.comment.clear();
                        self.state = State::CommentStart;
                    } else if self.starts_with_ci(b"doctype") {
                        self.pos += 7;
                        self.state = State::Doctype;
                    } else if self.allow_cdata && self.starts_with(b"[CDATA[") {
                        self.pos += 7;
                        self.state = State::CdataSection;
                    } else {
                        self.comment.clear();
                        self.state = State::BogusComment;
                    }
                }
                State::CommentStart => match b {
                    b'-' => self.state = State::CommentStartDash,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_comment();
                    }
                    _ => {
                        self.pos -= 1;
                        self.state = State::Comment;
                    }
                },
                State::CommentStartDash => match b {
                    b'-' => self.state = State::CommentEnd,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_comment();
                    }
                    _ => {
                        self.comment.push(b'-');
                        self.pos -= 1;
                        self.state = State::Comment;
                    }
                },
                State::Comment => match b {
                    b'-' => self.state = State::CommentEndDash,
                    0 => push_char(&mut self.comment, REPLACEMENT),
                    _ => {
                        let start = self.pos - 1;
                        let mut end = self.pos;
                        while end < self.input.len() && !matches!(self.input[end], b'-' | 0) {
                            end += 1;
                        }
                        self.comment.extend_from_slice(&self.input[start..end]);
                        self.pos = end;
                    }
                },
                State::CommentEndDash => match b {
                    b'-' => self.state = State::CommentEnd,
                    _ => {
                        self.comment.push(b'-');
                        self.pos -= 1;
                        self.state = State::Comment;
                    }
                },
                State::CommentEnd => match b {
                    b'>' => {
                        self.state = State::Data;
                        self.emit_comment();
                    }
                    b'!' => self.state = State::CommentEndBang,
                    b'-' => self.comment.push(b'-'),
                    _ => {
                        self.comment.extend_from_slice(b"--");
                        self.pos -= 1;
                        self.state = State::Comment;
                    }
                },
                State::CommentEndBang => match b {
                    b'-' => {
                        self.comment.extend_from_slice(b"--!");
                        self.state = State::CommentEndDash;
                    }
                    b'>' => {
                        self.state = State::Data;
                        self.emit_comment();
                    }
                    _ => {
                        self.comment.extend_from_slice(b"--!");
                        self.pos -= 1;
                        self.state = State::Comment;
                    }
                },
                State::Doctype => {
                    self.new_doctype();
                    if !is_space(b) {
                        self.pos -= 1;
                    }
                    self.state = State::BeforeDoctypeName;
                }
                State::BeforeDoctypeName => match b {
                    b if is_space(b) => {}
                    b'>' => {
                        self.dt_quirks = true;
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    _ => {
                        let mut n = Vec::new();
                        if b == 0 {
                            push_char(&mut n, REPLACEMENT);
                        } else {
                            n.push(b.to_ascii_lowercase());
                        }
                        self.dt_name = Some(n);
                        self.state = State::DoctypeName;
                    }
                },
                State::DoctypeName => match b {
                    b if is_space(b) => self.state = State::AfterDoctypeName,
                    b'>' => {
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    _ => {
                        if let Some(n) = self.dt_name.as_mut() {
                            if b == 0 {
                                push_char(n, REPLACEMENT);
                            } else {
                                n.push(b.to_ascii_lowercase());
                            }
                        }
                    }
                },
                State::AfterDoctypeName => match b {
                    b if is_space(b) => {}
                    b'>' => {
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    _ => {
                        self.pos -= 1;
                        if self.starts_with_ci(b"public") {
                            self.pos += 6;
                            self.dt_public_next = true;
                            self.state = State::BeforeDoctypeId;
                        } else if self.starts_with_ci(b"system") {
                            self.pos += 6;
                            self.dt_public_next = false;
                            self.state = State::BeforeDoctypeId;
                        } else {
                            self.pos += 1;
                            self.dt_quirks = true;
                            self.state = State::BogusDoctype;
                        }
                    }
                },
                State::BeforeDoctypeId => match b {
                    b if is_space(b) => {}
                    b'"' | b'\'' => {
                        if self.dt_public_next {
                            self.dt_public = Some(Vec::new());
                        } else {
                            self.dt_system = Some(Vec::new());
                        }
                        self.state = if b == b'"' {
                            State::DoctypeIdDq
                        } else {
                            State::DoctypeIdSq
                        };
                    }
                    b'>' => {
                        self.dt_quirks = true;
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    _ => {
                        self.dt_quirks = true;
                        self.state = State::BogusDoctype;
                    }
                },
                State::DoctypeIdDq | State::DoctypeIdSq => {
                    let q = if self.state == State::DoctypeIdDq {
                        b'"'
                    } else {
                        b'\''
                    };
                    if b == q {
                        self.state = State::AfterDoctypeId;
                    } else if b == b'>' {
                        self.dt_quirks = true;
                        self.state = State::Data;
                        self.emit_doctype();
                    } else {
                        let id = if self.dt_public_next {
                            &mut self.dt_public
                        } else {
                            &mut self.dt_system
                        };
                        if let Some(v) = id.as_mut() {
                            if b == 0 {
                                push_char(v, REPLACEMENT);
                            } else {
                                v.push(b);
                            }
                        }
                    }
                }
                State::AfterDoctypeId => match b {
                    b if is_space(b) => {}
                    b'>' => {
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    b'"' | b'\'' if self.dt_public_next => {
                        // The system id after a public one.
                        self.dt_public_next = false;
                        self.dt_system = Some(Vec::new());
                        self.state = if b == b'"' {
                            State::DoctypeIdDq
                        } else {
                            State::DoctypeIdSq
                        };
                    }
                    _ => {
                        self.state = State::BogusDoctype;
                    }
                },
                State::BogusDoctype => {
                    if b == b'>' {
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                }
                State::CdataSection => {
                    if b == b']' && self.starts_with(b"]>") {
                        self.pos += 2;
                        self.state = State::Data;
                    } else if b == 0 {
                        push_char(&mut self.text, REPLACEMENT);
                    } else {
                        self.text.push(b);
                    }
                }
            }
        }
    }

    /// End of input in the current state (§13.2.5, each state's "EOF" entry).
    fn at_eof(&mut self) {
        match self.state {
            State::TagOpen => self.text.push(b'<'),
            State::EndTagOpen => self.text.extend_from_slice(b"</"),
            State::RawLessThan => self.text.push(b'<'),
            State::RawEndTagOpen => self.text.extend_from_slice(b"</"),
            State::RawEndTagName => {
                self.text.extend_from_slice(b"</");
                let t = core::mem::take(&mut self.temp);
                self.text.extend_from_slice(&t);
            }
            State::BogusComment
            | State::CommentStart
            | State::CommentStartDash
            | State::Comment
            | State::CommentEndDash
            | State::CommentEnd
            | State::CommentEndBang => {
                self.state = State::Data;
                self.emit_comment();
                return;
            }
            State::MarkupDeclarationOpen => {
                self.state = State::Data;
                self.emit_comment();
                return;
            }
            State::Doctype
            | State::BeforeDoctypeName
            | State::DoctypeName
            | State::AfterDoctypeName
            | State::BeforeDoctypeId
            | State::DoctypeIdDq
            | State::DoctypeIdSq
            | State::AfterDoctypeId => {
                if self.state == State::Doctype {
                    self.new_doctype();
                }
                self.dt_quirks = true;
                self.state = State::Data;
                self.emit_doctype();
                return;
            }
            State::BogusDoctype => {
                self.state = State::Data;
                self.emit_doctype();
                return;
            }
            // In a tag: the tag is dropped (§13.2.5.8 and friends: "eof-in-tag").
            _ => {}
        }
        self.state = State::Data;
        self.eof_sent = true;
        self.emit(Token::Eof);
    }

    /// §13.2.5.72 "Character reference state", entered on '&'. Appends the result to the
    /// attribute value (`in_attr`) or the text run.
    fn char_ref(&mut self, in_attr: bool) {
        let rest = &self.input[self.pos..];
        let out_char = |t: &mut Self, c: char| {
            if in_attr {
                push_char(&mut t.attr_value, c)
            } else {
                push_char(&mut t.text, c)
            }
        };
        let out_bytes = |t: &mut Self, b: &[u8]| {
            if in_attr {
                t.attr_value.extend_from_slice(b)
            } else {
                t.text.extend_from_slice(b)
            }
        };
        match rest.first() {
            Some(b'#') => {
                // §13.2.5.75-80: numeric character reference.
                let (hex, digits_at) = match rest.get(1) {
                    Some(b'x') | Some(b'X') => (true, 2),
                    _ => (false, 1),
                };
                let mut i = digits_at;
                let mut v: u32 = 0;
                let mut any = false;
                while let Some(&d) = rest.get(i) {
                    let dv = if hex {
                        (d as char).to_digit(16)
                    } else {
                        (d as char).to_digit(10)
                    };
                    let Some(dv) = dv else { break };
                    any = true;
                    v = v
                        .saturating_mul(if hex { 16 } else { 10 })
                        .saturating_add(dv);
                    i += 1;
                }
                if !any {
                    // "absence-of-digits-in-numeric-character-reference": the & is literal.
                    out_bytes(self, b"&");
                    return;
                }
                if rest.get(i) == Some(&b';') {
                    i += 1;
                }
                self.pos += i;
                out_char(self, numeric_ref(v));
            }
            Some(c) if c.is_ascii_alphanumeric() => {
                // §13.2.5.73: the longest name in the table that matches.
                let mut n = 0;
                while n < MAX_LEN && n < rest.len() && rest[n].is_ascii_alphanumeric() {
                    n += 1;
                }
                let semi = rest.get(n) == Some(&b';');
                let mut found: Option<(usize, [u32; 2])> = None;
                let mut len = if semi { n + 1 } else { n };
                while len > 0 {
                    if let Ok(i) = ENTITIES.binary_search_by(|e| e.0.as_bytes().cmp(&rest[..len])) {
                        found = Some((len, ENTITIES[i].1));
                        break;
                    }
                    len -= 1;
                }
                match found {
                    Some((len, cps)) => {
                        let ends_semi = rest[len - 1] == b';';
                        let next = rest.get(len).copied();
                        if in_attr
                            && !ends_semi
                            && next.is_some_and(|b| b == b'=' || b.is_ascii_alphanumeric())
                        {
                            // Historical: "&copy=" in an attribute stays as written.
                            out_bytes(self, b"&");
                            return;
                        }
                        self.pos += len;
                        for cp in cps {
                            if cp != 0 {
                                out_char(self, char::from_u32(cp).unwrap_or(REPLACEMENT));
                            }
                        }
                    }
                    None => out_bytes(self, b"&"),
                }
            }
            _ => out_bytes(self, b"&"),
        }
    }
}

/// §13.2.5.80 "Numeric character reference end state": the code point a `&#...;` stands for.
fn numeric_ref(v: u32) -> char {
    const C1: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    let v = match v {
        0 => 0xFFFD,
        0x80..=0x9F => C1[(v - 0x80) as usize],
        0xD800..=0xDFFF => 0xFFFD,
        v if v > 0x10FFFF => 0xFFFD,
        v => v,
    };
    char::from_u32(v).unwrap_or(REPLACEMENT)
}
