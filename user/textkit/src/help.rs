//! The help lists, laid out the way `files help` is: a usage line, then one line per
//! command (two spaces, the command padded to 24 columns, what it does), then notes.

pub const TEXT: &str = "\
usage: text COMMAND ...    read, search, sort and count lines of text

  show FILE...            print files one after another (--numbers: number the lines)
  first [N] FILE...       the first N lines (10 without N)
  last [N] FILE...        the last N lines (10 without N)
  find TEXT [FILE...]     the lines with TEXT in them: --ignore-case --invert --numbers
  sort [FILE...]          lines in order: --reverse --numbers --unique --field N
  unique [FILE...]        drop a line that repeats the one before: --count --repeated
  count [FILE...]         lines, words and bytes, as a table: --lines --words --bytes
  help                    this list

With no FILE (or -) each one reads what's piped in: files list | text find .txt.
find looks for plain text (not a pattern) and exits 1 when nothing matches; --invert
keeps the lines without it, --count says how many, --names lists the files with a match.
count --lines (or --words, --bytes) on one input prints just the number.
";

pub const PRINT: &str = "\
usage: print [--no-newline] [--escapes] WORDS...    print words, then a newline

  --no-newline (-n)       no newline at the end
  --escapes (-e)          \\n newline, \\t tab, \\e escape (colours), \\\\ a backslash, \\c stop
  --                      print everything after it as it is, even --escapes
  --help                  this list
";

pub const PAUSE: &str = "\
usage: pause DURATION    wait, then carry on (Ctrl-C stops it early)

  pause 5                 five seconds (5s is the same)
  pause 500ms             half a second (0.5 works too)
  pause 2m                two minutes (1h: an hour)
  help                    this list
";

pub const VARIABLES: &str = "\
usage: variables COMMAND ...    the shell's variables (built into msh)

  list                    every variable: its name, value, and whether programs get it
  get NAME                print NAME's value
  set NAME=value...       set them for this shell and every program it starts
  remove NAME...          forget them
  run NAME=value... CMD   run CMD with those set, for CMD only
  help                    this list

NAME=value on its own sets a variable for this shell only; variables set NAME passes it
on to programs too. $NAME is its value. variables on its own is variables list.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        for h in [TEXT, PRINT, PAUSE, VARIABLES] {
            let mut lines = h.lines();
            assert!(lines.next().unwrap().starts_with("usage: "));
            assert_eq!(lines.next(), Some(""));
            let cmds: alloc::vec::Vec<&str> = lines.take_while(|l| !l.is_empty()).collect();
            assert!(cmds.len() >= 4);
            for l in cmds {
                assert!(l.starts_with("  ") && !l.starts_with("   "), "{l}");
                let (cmd, what) = l[2..].split_at(24);
                assert!(cmd.ends_with(' ') && !cmd.trim_end().contains("  "), "{l}");
                assert!(!what.starts_with(' '), "{l}");
                assert!(l.len() <= 92, "{l}");
            }
            assert!(h.ends_with('\n'));
        }
        assert!(TEXT.contains("\n  help                    this list\n"));
        for v in crate::lines::VERBS {
            assert!(TEXT.contains(&alloc::format!("\n  {v} ")), "{v}");
        }
    }
}
