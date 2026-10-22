use crate::{Buffer, Eol, FindOpts, Pos, TextError};

fn buf(s: &str) -> Buffer {
    Buffer::from_bytes(s.as_bytes()).unwrap()
}

fn p(l: usize, c: usize) -> Pos {
    Pos::new(l, c)
}

fn typed(b: &mut Buffer, s: &str) {
    for c in s.chars() {
        if c == '\n' {
            b.newline();
        } else {
            b.insert_char(c);
        }
    }
}

// ---- loading and saving ---------------------------------------------------------------

#[test]
fn empty_and_roundtrip() {
    let b = Buffer::new();
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.to_bytes(), b"\n"[..0].to_vec()); // empty buffer, empty file
    for s in [
        "",
        "a",
        "a\n",
        "a\nb",
        "a\nb\n",
        "\n",
        "\n\n",
        "a\n\nb\n",
        "tab\there\n",
        "héllo\n日本語\n",
    ] {
        assert_eq!(buf(s).to_bytes(), s.as_bytes(), "roundtrip {s:?}");
    }
}

#[test]
fn lines_and_counts() {
    let b = buf("one\ntwo\n\nfour");
    assert_eq!(b.line_count(), 4);
    assert_eq!(b.line(1), "two");
    assert_eq!(b.line(2), "");
    assert_eq!(b.line(9), "");
    assert_eq!(b.char_count(), 3 + 3 + 4 + 3);
    assert_eq!(buf("a\n").line_count(), 1);
    assert_eq!(buf("a\n\n").line_count(), 2);
    assert_eq!(b.gutter_digits(), 1);
    assert_eq!(buf(&"x\n".repeat(120)).gutter_digits(), 3);
}

#[test]
fn crlf_is_kept() {
    let b = buf("a\r\nb\r\nc\r\n");
    assert_eq!(b.eol(), Eol::Crlf);
    assert_eq!(b.line_count(), 3);
    assert_eq!(b.line(0), "a");
    assert_eq!(b.to_bytes(), b"a\r\nb\r\nc\r\n");
    let mut b = b;
    b.set_cursor(p(2, 1), false);
    b.newline();
    typed(&mut b, "d");
    assert_eq!(b.to_bytes(), b"a\r\nb\r\nc\r\nd\r\n");
    b.set_eol(Eol::Lf);
    assert_eq!(b.to_bytes(), b"a\nb\nc\nd\n");
}

#[test]
fn mixed_endings_follow_the_majority() {
    assert_eq!(buf("a\nb\nc\r\n").eol(), Eol::Lf);
    assert_eq!(buf("a\r\nb\r\nc\n").eol(), Eol::Crlf);
}

#[test]
fn final_newline_is_kept_either_way() {
    assert_eq!(buf("a\nb").to_bytes(), b"a\nb");
    assert_eq!(buf("a\nb\n").to_bytes(), b"a\nb\n");
}

#[test]
fn bom_is_kept() {
    let b = Buffer::from_bytes(b"\xEF\xBB\xBFhi\n").unwrap();
    assert_eq!(b.line(0), "hi");
    assert_eq!(b.to_bytes(), b"\xEF\xBB\xBFhi\n");
}

#[test]
fn refuses_what_is_not_utf8_text() {
    assert_eq!(Buffer::from_bytes(b"a\0b").err(), Some(TextError::Binary));
    assert_eq!(
        Buffer::from_bytes(b"\xFF\xFEh\0i\0").err(),
        Some(TextError::Utf16)
    );
    assert_eq!(
        Buffer::from_bytes(b"ok\xC3\x28").err(),
        Some(TextError::InvalidUtf8(2))
    );
    assert_eq!(
        Buffer::from_bytes(b"\xEF\xBB\xBFok\xFF").err(),
        Some(TextError::InvalidUtf8(5))
    );
    // Truncated multi-byte char at the end.
    assert!(matches!(
        Buffer::from_bytes(b"a\xE6\x97").err(),
        Some(TextError::InvalidUtf8(1))
    ));
}

#[test]
fn lossy_open_says_so() {
    let b = Buffer::from_bytes_lossy(b"ok\xFFgo\n");
    assert!(b.is_lossy());
    assert_eq!(b.line(0), "ok\u{FFFD}go");
    assert!(!Buffer::from_bytes_lossy(b"fine\n").is_lossy());
}

// ---- typing and cursor ----------------------------------------------------------------

#[test]
fn typing_and_columns_count_chars() {
    let mut b = Buffer::new();
    typed(&mut b, "héllo 日本");
    assert_eq!(b.cursor(), p(0, 8));
    assert_eq!(b.line(0), "héllo 日本");
    b.move_left(false);
    b.backspace();
    assert_eq!(b.line(0), "héllo 本");
    assert_eq!(b.cursor(), p(0, 6));
    b.set_cursor(p(0, 1), false);
    b.delete_forward();
    assert_eq!(b.line(0), "hllo 本");
}

#[test]
fn newline_splits_and_backspace_joins() {
    let mut b = buf("hello world");
    b.set_cursor(p(0, 5), false);
    b.newline();
    assert_eq!(b.line(0), "hello");
    assert_eq!(b.line(1), " world");
    assert_eq!(b.cursor(), p(1, 0));
    b.backspace();
    assert_eq!(b.line(0), "hello world");
    assert_eq!(b.cursor(), p(0, 5));
    b.doc_end_cursor(false);
    b.delete_forward(); // at the very end: nothing
    assert_eq!(b.text(), "hello world");
    b.doc_start_cursor(false);
    b.backspace(); // at the very start: nothing
    assert_eq!(b.text(), "hello world");
}

#[test]
fn delete_at_line_end_joins_the_next_line() {
    let mut b = buf("ab\ncd\n");
    b.set_cursor(p(0, 2), false);
    b.delete_forward();
    assert_eq!(b.text(), "abcd");
}

#[test]
fn auto_indent_copies_leading_whitespace() {
    let mut b = buf("    if x {");
    b.doc_end_cursor(false);
    b.newline();
    assert_eq!(b.line(1), "    ");
    assert_eq!(b.cursor(), p(1, 4));
    b.auto_indent = false;
    b.newline();
    assert_eq!(b.line(2), "");
    // Enter in the middle of the indentation only copies what is left of the cursor.
    let mut b = buf("\t\tx");
    b.set_cursor(p(0, 1), false);
    b.newline();
    assert_eq!(b.line(1), "\t\tx");
    assert_eq!(b.line(0), "\t");
}

#[test]
fn vertical_moves_remember_the_column() {
    let mut b = buf("long line here\nab\nanother long one");
    b.set_cursor(p(0, 10), false);
    b.move_lines(1, false);
    assert_eq!(b.cursor(), p(1, 2));
    b.move_lines(1, false);
    assert_eq!(b.cursor(), p(2, 10));
    b.move_lines(1, false); // past the last line: end
    assert_eq!(b.cursor(), p(2, 16));
    b.move_lines(-1, false);
    b.move_lines(-1, false);
    b.move_lines(-1, false); // before the first: start
    assert_eq!(b.cursor(), p(0, 0));
}

#[test]
fn left_right_cross_lines() {
    let mut b = buf("ab\ncd");
    b.set_cursor(p(0, 2), false);
    b.move_right(false);
    assert_eq!(b.cursor(), p(1, 0));
    b.move_left(false);
    assert_eq!(b.cursor(), p(0, 2));
}

#[test]
fn home_toggles_between_indent_and_column_zero() {
    let mut b = buf("   text");
    b.set_cursor(p(0, 6), false);
    b.home(false);
    assert_eq!(b.cursor(), p(0, 3));
    b.home(false);
    assert_eq!(b.cursor(), p(0, 0));
    b.home(false);
    assert_eq!(b.cursor(), p(0, 3));
    b.end(false);
    assert_eq!(b.cursor(), p(0, 7));
}

#[test]
fn word_moves() {
    let mut b = buf("foo bar_baz  qux\nnext");
    b.move_word_right(false);
    assert_eq!(b.cursor(), p(0, 3));
    b.move_word_right(false);
    assert_eq!(b.cursor(), p(0, 11));
    b.move_word_right(false);
    b.move_word_right(false); // end of line, then the next line
    assert_eq!(b.cursor(), p(1, 0));
    b.move_word_left(false);
    assert_eq!(b.cursor(), p(0, 16));
    b.move_word_left(false);
    assert_eq!(b.cursor(), p(0, 13));
}

#[test]
fn double_click_selects_word_or_run() {
    let mut b = buf("foo  bar.baz");
    b.select_word_at(p(0, 1));
    assert_eq!(b.selected_text().unwrap(), "foo");
    b.select_word_at(p(0, 3));
    assert_eq!(b.selected_text().unwrap(), "  ");
    b.select_word_at(p(0, 8));
    assert_eq!(b.selected_text().unwrap(), ".");
    b.select_word_at(p(0, 40)); // past the end: the last word
    assert_eq!(b.selected_text().unwrap(), "baz");
}

// ---- selection ------------------------------------------------------------------------

#[test]
fn shift_selects_and_typing_replaces() {
    let mut b = buf("hello world");
    b.move_word_right(true);
    assert_eq!(b.selection(), Some((p(0, 0), p(0, 5))));
    assert_eq!(b.selected_text().unwrap(), "hello");
    b.insert_str("goodbye");
    assert_eq!(b.text(), "goodbye world");
    assert_eq!(b.selection(), None);
    assert_eq!(b.cursor(), p(0, 7));
}

#[test]
fn backwards_selection_is_normalised() {
    let mut b = buf("abcdef");
    b.set_cursor(p(0, 4), false);
    b.set_cursor(p(0, 1), true);
    assert_eq!(b.selection(), Some((p(0, 1), p(0, 4))));
    assert_eq!(b.selected_text().unwrap(), "bcd");
}

#[test]
fn arrow_without_shift_collapses_to_the_edge() {
    let mut b = buf("abcdef");
    b.select(p(0, 1), p(0, 4));
    b.move_left(false);
    assert_eq!(b.cursor(), p(0, 1));
    b.select(p(0, 1), p(0, 4));
    b.move_right(false);
    assert_eq!(b.cursor(), p(0, 4));
    assert_eq!(b.selection(), None);
}

#[test]
fn multi_line_selection_text_and_cut() {
    let mut b = buf("one\ntwo\nthree");
    b.select(p(0, 2), p(2, 2));
    assert_eq!(b.selected_text().unwrap(), "e\ntwo\nth");
    assert_eq!(b.cut().unwrap(), "e\ntwo\nth");
    assert_eq!(b.text(), "onree");
    assert_eq!(b.cursor(), p(0, 2));
    assert!(b.undo());
    assert_eq!(b.text(), "one\ntwo\nthree");
    assert_eq!(b.selection(), Some((p(0, 2), p(2, 2))));
}

#[test]
fn select_all_and_delete() {
    let mut b = buf("a\nb\nc\n");
    b.select_all();
    b.backspace();
    assert_eq!(b.text(), "");
    assert_eq!(b.line_count(), 1);
    assert!(b.undo());
    assert_eq!(b.text(), "a\nb\nc");
}

#[test]
fn paste_with_line_breaks_and_crs() {
    let mut b = buf("[]");
    b.set_cursor(p(0, 1), false);
    b.insert_str("x\r\ny\rz\nw");
    assert_eq!(b.text(), "[x\ny\nz\nw]");
    assert_eq!(b.cursor(), p(3, 1));
    assert!(b.undo());
    assert_eq!(b.text(), "[]");
}

#[test]
fn clamping_of_wild_positions() {
    let mut b = buf("ab\ncd");
    b.set_cursor(p(99, 99), false);
    assert_eq!(b.cursor(), p(1, 2));
    assert_eq!(b.slice(p(0, 1), p(50, 50)), "b\ncd");
    assert_eq!(b.slice(p(1, 1), p(0, 1)), "b\nc"); // reversed
    b.goto_line(1);
    assert_eq!(b.cursor(), p(0, 0));
    b.goto_line(0);
    assert_eq!(b.cursor(), p(0, 0));
    b.goto_line(999);
    assert_eq!(b.cursor().line, 1);
}

// ---- undo / redo ----------------------------------------------------------------------

#[test]
fn typing_is_one_undo_step() {
    let mut b = Buffer::new();
    typed(&mut b, "hello");
    assert_eq!(b.text(), "hello");
    assert!(b.undo());
    assert_eq!(b.text(), "");
    assert_eq!(b.cursor(), p(0, 0));
    assert!(!b.undo());
    assert!(b.redo());
    assert_eq!(b.text(), "hello");
    assert_eq!(b.cursor(), p(0, 5));
    assert!(!b.redo());
}

#[test]
fn moving_the_cursor_breaks_the_group() {
    let mut b = Buffer::new();
    typed(&mut b, "ab");
    b.set_cursor(p(0, 0), false);
    typed(&mut b, "X");
    assert_eq!(b.text(), "Xab");
    b.undo();
    assert_eq!(b.text(), "ab");
    b.undo();
    assert_eq!(b.text(), "");
}

#[test]
fn backspaces_group_together_but_not_with_typing() {
    let mut b = buf("hello");
    b.doc_end_cursor(false);
    b.backspace();
    b.backspace();
    typed(&mut b, "p");
    assert_eq!(b.text(), "help");
    b.undo();
    assert_eq!(b.text(), "hel");
    b.undo();
    assert_eq!(b.text(), "hello");
    assert_eq!(b.cursor(), p(0, 5));
}

#[test]
fn forward_deletes_group_together() {
    let mut b = buf("hello");
    b.delete_forward();
    b.delete_forward();
    assert_eq!(b.text(), "llo");
    b.undo();
    assert_eq!(b.text(), "hello");
}

#[test]
fn newline_is_its_own_step() {
    let mut b = Buffer::new();
    typed(&mut b, "ab\ncd");
    assert_eq!(b.text(), "ab\ncd");
    b.undo();
    assert_eq!(b.text(), "ab\n");
    b.undo();
    assert_eq!(b.text(), "ab");
    b.undo();
    assert_eq!(b.text(), "");
}

#[test]
fn a_new_edit_clears_redo() {
    let mut b = Buffer::new();
    typed(&mut b, "a");
    b.undo();
    assert!(b.can_redo());
    typed(&mut b, "b");
    assert!(!b.can_redo());
    assert_eq!(b.text(), "b");
}

#[test]
fn undo_restores_the_selection_too() {
    let mut b = buf("hello world");
    b.select(p(0, 6), p(0, 11));
    b.insert_str("there");
    assert_eq!(b.text(), "hello there");
    b.undo();
    assert_eq!(b.text(), "hello world");
    assert_eq!(b.selection(), Some((p(0, 6), p(0, 11))));
    b.redo();
    assert_eq!(b.text(), "hello there");
    assert_eq!(b.cursor(), p(0, 11));
}

#[test]
fn dirty_tracks_the_saved_state() {
    let mut b = buf("x");
    assert!(!b.is_dirty());
    b.doc_end_cursor(false);
    typed(&mut b, "y");
    assert!(b.is_dirty());
    b.undo();
    assert!(!b.is_dirty());
    b.redo();
    assert!(b.is_dirty());
    b.mark_saved();
    assert!(!b.is_dirty());
    // Typing straight after a save must not merge into the saved group.
    typed(&mut b, "z");
    assert!(b.is_dirty());
    b.undo();
    assert!(!b.is_dirty());
    b.undo();
    assert!(b.is_dirty());
    b.redo();
    assert!(!b.is_dirty());
}

#[test]
fn an_empty_new_buffer_is_clean_and_changing_eol_dirties() {
    let mut b = Buffer::new();
    assert!(!b.is_dirty());
    b.set_eol(Eol::Crlf);
    assert!(b.is_dirty());
    b.mark_saved();
    assert!(!b.is_dirty());
}

#[test]
fn history_is_capped() {
    let mut b = Buffer::new();
    for i in 0..(crate::MAX_UNDO + 50) {
        b.insert_str(&format!("{}\n", i % 10));
    }
    let mut n = 0;
    while b.undo() {
        n += 1;
    }
    assert_eq!(n, crate::MAX_UNDO);
    assert!(b.is_dirty());
}

// ---- line operations ------------------------------------------------------------------

#[test]
fn delete_lines() {
    let mut b = buf("a\nb\nc\nd");
    b.set_cursor(p(1, 1), false);
    b.delete_lines();
    assert_eq!(b.text(), "a\nc\nd");
    assert_eq!(b.cursor(), p(1, 0));
    b.set_cursor(p(2, 0), false);
    b.delete_lines(); // last line takes the newline before it
    assert_eq!(b.text(), "a\nc");
    b.select(p(0, 0), p(1, 0)); // ends at column 0: only line 0 is touched
    b.delete_lines();
    assert_eq!(b.text(), "c");
    b.delete_lines();
    assert_eq!(b.text(), "");
    b.undo();
    assert_eq!(b.text(), "c");
}

#[test]
fn indent_and_dedent() {
    let mut b = buf("a\n  b\n\tc\nd");
    b.select(p(0, 0), p(2, 1));
    assert!(b.indent_lines("    "));
    assert_eq!(b.text(), "    a\n      b\n    \tc\nd");
    assert_eq!(b.selection(), Some((p(0, 4), p(2, 5))));
    b.dedent_lines(4);
    assert_eq!(b.text(), "a\n  b\n\tc\nd");
    b.undo();
    assert_eq!(b.text(), "    a\n      b\n    \tc\nd");
    b.undo();
    assert_eq!(b.text(), "a\n  b\n\tc\nd");
    // One line, or no selection: not handled, caller inserts a tab.
    b.set_cursor(p(0, 0), false);
    assert!(!b.indent_lines("  "));
    b.select(p(0, 0), p(0, 1));
    assert!(!b.indent_lines("  "));
    // Dedent the cursor's own line.
    b.set_cursor(p(1, 2), false);
    b.dedent_lines(4);
    assert_eq!(b.line(1), "b");
    assert_eq!(b.cursor(), p(1, 0));
    b.dedent_lines(4); // nothing left to remove
    assert_eq!(b.line(1), "b");
}

// ---- find and replace -----------------------------------------------------------------

const CS: FindOpts = FindOpts {
    case_sensitive: true,
    whole_word: false,
};
const CI: FindOpts = FindOpts {
    case_sensitive: false,
    whole_word: false,
};
const WW: FindOpts = FindOpts {
    case_sensitive: true,
    whole_word: true,
};

#[test]
fn find_next_and_wrap() {
    let b = buf("cat dog cat\nbird\ncat");
    assert_eq!(b.find_next("cat", CS, p(0, 0)), Some((p(0, 0), p(0, 3))));
    assert_eq!(b.find_next("cat", CS, p(0, 1)), Some((p(0, 8), p(0, 11))));
    assert_eq!(b.find_next("cat", CS, p(0, 9)), Some((p(2, 0), p(2, 3))));
    assert_eq!(b.find_next("cat", CS, p(2, 1)), Some((p(0, 0), p(0, 3)))); // wrapped
    assert_eq!(b.find_next("fish", CS, p(0, 0)), None);
    assert_eq!(b.find_next("", CS, p(0, 0)), None);
    assert_eq!(b.find_next("a\nb", CS, p(0, 0)), None);
}

#[test]
fn find_prev_and_wrap() {
    let b = buf("cat dog cat\nbird\ncat");
    assert_eq!(b.find_prev("cat", CS, p(2, 3)), Some((p(2, 0), p(2, 3))));
    assert_eq!(b.find_prev("cat", CS, p(2, 0)), Some((p(0, 8), p(0, 11))));
    assert_eq!(b.find_prev("cat", CS, p(0, 2)), Some((p(2, 0), p(2, 3)))); // wrapped
    assert_eq!(b.find_prev("zzz", CS, p(0, 2)), None);
}

#[test]
fn single_match_is_found_again_after_wrapping() {
    let b = buf("only one here");
    assert_eq!(b.find_next("one", CS, p(0, 5)), Some((p(0, 5), p(0, 8))));
    assert_eq!(b.find_next("one", CS, p(0, 6)), Some((p(0, 5), p(0, 8))));
    assert_eq!(b.find_prev("one", CS, p(0, 5)), Some((p(0, 5), p(0, 8))));
}

#[test]
fn case_and_whole_word() {
    let b = buf("Cat cat CAT concat cat_x");
    assert_eq!(b.count_matches("cat", CS), 3);
    assert_eq!(b.count_matches("cat", CI), 5);
    assert_eq!(b.count_matches("cat", WW), 1);
    assert_eq!(
        b.count_matches(
            "CAT",
            FindOpts {
                case_sensitive: false,
                whole_word: true
            }
        ),
        3
    );
    let b = buf("ÉCOLE école");
    assert_eq!(b.count_matches("école", CI), 2);
    assert_eq!(b.count_matches("école", CS), 1);
}

#[test]
fn matches_do_not_overlap() {
    let b = buf("aaaa");
    assert_eq!(b.count_matches("aa", CS), 2);
    let mut b = b;
    assert_eq!(b.replace_all("aa", "b", CS), 2);
    assert_eq!(b.text(), "bb");
}

#[test]
fn find_select_walks_through_matches() {
    let mut b = buf("x1 x2\nx3");
    assert!(b.find_select("x", CS, false));
    assert_eq!(b.selection(), Some((p(0, 0), p(0, 1))));
    assert!(b.find_select("x", CS, false));
    assert_eq!(b.selection(), Some((p(0, 3), p(0, 4))));
    assert!(b.find_select("x", CS, false));
    assert_eq!(b.selection(), Some((p(1, 0), p(1, 1))));
    assert!(b.find_select("x", CS, false));
    assert_eq!(b.selection(), Some((p(0, 0), p(0, 1)))); // wrapped
    assert!(b.find_select("x", CS, true));
    assert_eq!(b.selection(), Some((p(1, 0), p(1, 1))));
    assert!(!b.find_select("q", CS, false));
}

#[test]
fn replace_current_then_next() {
    let mut b = buf("a-a-a");
    assert!(!b.replace_current("a", "XY", CS)); // nothing selected yet: just finds the first
    assert_eq!(b.selection(), Some((p(0, 0), p(0, 1))));
    assert!(b.replace_current("a", "XY", CS));
    assert_eq!(b.text(), "XY-a-a");
    assert_eq!(b.selection(), Some((p(0, 3), p(0, 4))));
    assert!(b.replace_current("a", "XY", CS));
    assert!(b.replace_current("a", "XY", CS) || b.text() == "XY-XY-XY");
    assert_eq!(b.text(), "XY-XY-XY");
}

#[test]
fn replace_all_is_one_undo_step() {
    let mut b = buf("one two one\ntwo one\n");
    assert_eq!(b.replace_all("one", "1", CS), 3);
    assert_eq!(b.text(), "1 two 1\ntwo 1");
    assert_eq!(b.replace_all("zzz", "-", CS), 0);
    assert_eq!(b.replace_all("", "-", CS), 0);
    assert!(b.undo());
    assert_eq!(b.text(), "one two one\ntwo one");
    assert!(b.redo());
    assert_eq!(b.text(), "1 two 1\ntwo 1");
}

#[test]
fn replace_with_longer_and_shorter_text_on_one_line() {
    let mut b = buf("ab ab ab");
    assert_eq!(b.replace_all("ab", "WXYZ", CS), 3);
    assert_eq!(b.text(), "WXYZ WXYZ WXYZ");
    assert_eq!(b.replace_all("WXYZ", "", CS), 3);
    assert_eq!(b.text(), "  ");
    b.undo();
    b.undo();
    assert_eq!(b.text(), "ab ab ab");
}

#[test]
fn replace_all_refuses_line_breaks() {
    let mut b = buf("a b");
    assert_eq!(b.replace_all("a", "x\ny", CS), 0);
    assert_eq!(b.text(), "a b");
}

// ---- a model check: random edits against a plain Vec<char> ------------------------------

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn model_offset(text: &[char], pos: Pos) -> usize {
    let mut off = 0;
    let mut line = 0;
    for (i, c) in text.iter().enumerate() {
        if line == pos.line {
            let col_end = text[i..].iter().take_while(|c| **c != '\n').count();
            return i + pos.col.min(col_end);
        }
        if *c == '\n' {
            line += 1;
        }
        off = i + 1;
    }
    let _ = off;
    // Last line (or past it).
    let start = text.iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
    start + pos.col.min(text.len() - start)
}

#[test]
fn random_edits_match_a_model_and_undo_redo_walks_back_and_forth() {
    const ALPHABET: &[char] = &['a', 'b', ' ', '\n', 'é', '日', 'x'];
    for seed in 1..=40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut b = Buffer::new();
        b.auto_indent = false;
        let mut model: Vec<char> = Vec::new();
        let mut states: Vec<String> = vec![String::new()];
        for _ in 0..120 {
            let line = rng.below(b.line_count() + 1);
            let col = rng.below(8);
            let p1 = Pos::new(line, col);
            let sel = rng.below(3) == 0;
            b.set_cursor(p1, false);
            if sel {
                let q = Pos::new(rng.below(b.line_count() + 1), rng.below(8));
                b.set_cursor(q, true);
            }
            let (a, z) = b.selection().unwrap_or((b.cursor(), b.cursor()));
            let (ma, mz) = (model_offset(&model, a), model_offset(&model, z));
            match rng.below(4) {
                0 | 1 => {
                    let n = 1 + rng.below(4);
                    let s: String = (0..n)
                        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
                        .collect();
                    b.insert_str(&s);
                    model.splice(ma..mz, s.chars());
                }
                2 => {
                    if b.selection().is_some() {
                        b.backspace();
                        model.drain(ma..mz);
                    } else if ma > 0 {
                        b.backspace();
                        model.remove(ma - 1);
                    }
                }
                _ => {
                    if b.selection().is_some() {
                        b.delete_forward();
                        model.drain(ma..mz);
                    } else if ma < model.len() {
                        b.delete_forward();
                        model.remove(ma);
                    }
                }
            }
            let text: String = model.iter().collect();
            assert_eq!(b.text(), text, "seed {seed}");
            assert_eq!(b.char_count(), model.len(), "seed {seed}");
            if states.last() != Some(&text) {
                states.push(text);
            }
        }
        // Walk the whole history back: each state we pass must be one we were in.
        let mut seen: Vec<String> = vec![b.text()];
        while b.undo() {
            seen.push(b.text());
        }
        assert_eq!(b.text(), "", "seed {seed}: undo all the way");
        for s in &seen {
            assert!(
                states.contains(s),
                "seed {seed}: undo produced a state we never had: {s:?}"
            );
        }
        let mut again = 0;
        while b.redo() {
            again += 1;
        }
        assert_eq!(again, seen.len() - 1, "seed {seed}");
        assert_eq!(
            b.text(),
            *states.last().unwrap(),
            "seed {seed}: redo all the way"
        );
    }
}
