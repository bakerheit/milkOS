//! HTML: the tokenizer and tree builder (WHATWG HTML §13.2), plus the named character
//! references they use.

pub mod entities;
pub mod tokenizer;
pub mod tree;

pub use tree::{Parsed, parse_document, parse_document_with, parse_fragment};
