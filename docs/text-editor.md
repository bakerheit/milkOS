# Text Editor

`/Apps/texteditor/texteditor` (dock tile from `/Apps/texteditor/manifest`). The window is
`user/texteditor`; the buffer, undo, find, wrapping and colouring are the `user/textbuf` crate,
which has host tests (`scripts/textbuf-test.sh`, 58 of them, including a random-edit model check
and undo/redo walks). It follows docs/ui-concepts/SPEC-APPS.md, section 2. One file per window.

Start it with a path (`/Apps/texteditor/texteditor FILE`), open with Cmd-O (the Powerbox Open
panel), or drop a file on the window. Text is UTF-8 only; the line ending (LF or CRLF), a final
newline (or none) and a byte-order mark are kept as they were. Files over 4 MB are refused. A file
that is binary, UTF-16 or not valid UTF-8 is shown **read-only** with the bad bytes replaced, and
Save is off, so it can't be damaged.

## Keys (Cmd or Ctrl)

| Key | Does |
| --- | --- |
| O, S, Shift-S | open (panel), save, Save As (panel) |
| Z, Shift-Z or Y | undo, redo (typing groups until a 1 s pause, a newline or a cursor jump) |
| X, C, V, A | cut, copy, paste (the system clipboard, `/dev/clipboard`), select all |
| F, Option-F | find; find and replace. Return / Shift-Return: next / previous. Esc closes |
| G, Shift-G | next / previous match |
| L | go to line. Shift-L: line numbers on or off |
| Shift-W, Shift-T | soft wrap on or off, tab width 2 or 4 (also by clicking the status bar) |
| Q, W | quit (asks Save, Don't Save, Cancel when there are unsaved changes) |
| Up / Down | by screen row; Option: a page. Cmd-Up / Cmd-Down: start / end of the text |
| Left / Right | by char; Option: by word. Cmd-Left / Cmd-Right: start / end of the line |
| Tab, Shift-Tab | spaces to the next tab stop; with a multi-line selection, indent / dedent it |

The window system sends no Home, End or Page keys to apps yet, hence the Cmd-arrow and Option-arrow
forms. Double click selects a word, triple click a line, a click in the gutter a line, a drag
selects.

## What it draws

A gutter with line numbers, the current line, the selection, every find match (the current one
outlined), a box on a bracket and its partner, and colours by file extension: Rust, C, shell and
Markdown (five roles for code; Markdown headings and `**bold**` are bold, list markers coloured,
inline code tinted). Code files don't wrap; text and Markdown do. The status bar shows line and
column, encoding, line ending, tab width, wrap, language, and Saved / Edited / Read only.

Settings (tab width, line numbers) are in `~/Library/Text Editor/settings`.

## Limits worth knowing

* **Closing the window can't be refused.** The window system has hidden the window by the time the
  app hears about it, so unsaved work can't be asked about; it is kept in
  `~/Library/Text Editor/Unsaved/<name>` instead. (Cmd-Q and Cmd-W do ask.)
* No tabs inside the app: another file is another window.
* A file opened through the panel has no path the app can write back to, so Save asks where.
* Wrapping counts a tab as one char (a line of many tabs wraps late).
* The editor isn't sandboxed (it needs to save beside the file); Preview is.

Test: `scripts/texteditor-test.sh` boots a desktop, opens a Rust file, types, saves with Cmd-S and
reads the file back, undoes, finds, quits, and opens a prose file and a binary one.
