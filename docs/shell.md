# msh, the milk shell

`user/msh` (shell), `user/coreutils` (tools), `user/rt/src/table.rs` (the table format).

## Plain Unix shell

```sh
files list -l /Commands | sort -n -k5 | tail -3  # pipelines
echo hi > /tmp/f; echo there >> /tmp/f      # redirection: > >> < 2> 2>&1
grep milk /System/Settings/motd && echo found || echo no
X=1; export Y=2; echo "$X $Y ${X}0 $? $$" ~ # variables, $? $$ ~, '...' "..." quoting
files list /Commands/u*                          # globs (* ?) in the last path component
sleep 10 &                                  # background job: jobs, fg %1, wait
```

Ctrl-C kills the foreground job, never the shell: every pipeline runs in its own process
group, which owns the terminal (TIOCSPGRP) while it runs; the shell catches SIGINT itself.
Built-ins: `go where exit export unset set jobs fg wait source help`. Everything else is a
program on `$PATH` (`/Commands`). Details: comments at the top of `user/msh/src/main.rs`.

## Moving around: go and where

```sh
go Documents        # into a folder (relative to where you are)
go ..               # up one
go /Apps            # an absolute path
go                  # home
go back             # back to the folder you were in before the last go
go ./back           # a folder that is really called "back"
where               # print the folder you're in; works in pipes: where | cat, where > f
```

There is no `cd` or `pwd` (like `ls`, `cp` and `mv`, which `files` replaced): typing them
gives `command not found`. Errors are in plain English: `go: Documents doesn't exist`,
`go: notes.txt is a file, not a folder`, `go: you can't open /Users/root`. `go` sets `PWD`
and `OLDPWD` for programs that read them.

`where` on its own is the current folder; with arguments it is the table filter
(`ps | where rss > 100`, below). msh tells them apart by the arguments, so a bare `where`
in a pipeline prints the folder and never becomes a table stage.

## Structured pipes

Plain text pipes stay exactly as they are. On top of that, some commands can pass a
**table** (rows of named, typed columns) down a pipe, and a few shell built-ins work on
tables, Nushell style:

```sh
ps | where rss > 100 | sort-by name          # filter + sort, shown as an aligned table
ps | where name == msh | select pid rss
files list /Commands | where size > 100K | sort-by size -r | first 3
ps | sort-by cpu -r | first 1 | get name     # -> plain text: one value per line
ps | where state == R | count
files list / | to-json
cat /proc/threads | where PRIO > 1           # plain text with a header row works too
ps | grep msh                                # a text tool after ps: ps prints text
```

### Rules

1. **Producers** (`ps`, `files list`, `trash list`, `env`, ...) print normal text, unless the environment variable
   `MILK_TABLE=1` is set, then they print a table. They never guess from the fd.
2. **msh decides**, per pipeline stage, from the command names:
   * a stage whose next stage is a table command gets `MILK_TABLE=1`;
   * the table commands always read and write tables;
   * wherever a table would flow into something that isn't a table command (a text
     tool, or the end of the pipeline: the terminal or a `>` file), msh inserts an
     implicit `table` stage that renders it as aligned text.
   So `ps | grep msh` is a plain text pipe, and `ps | where ... | grep x` renders the
   table to text before `grep` sees it.
3. **Table commands** given text instead of a table treat the first line as a header and
   split every line on runs of spaces (numbers become ints). So any columnar text works:
   `/proc/threads`, `df`-style output, a `ps` run without msh.

### Table commands (msh built-ins, run as pipeline stages)

| command | does |
|---|---|
| `where COL OP VALUE` | keep rows; OP is `== != < <= > >=`, `=~` (contains), `!~` |
| `sort-by COL [-r]` | stable sort, numeric for int/size columns |
| `select COL...` | keep these columns, in this order |
| `first [N]`, `last [N]` | first / last N rows (default 1) |
| `get COL` | the column's values, one per line (text) |
| `count` | number of rows (text) |
| `table` | render as aligned text (what msh inserts automatically) |
| `to-json` | rows as a JSON array of objects (text) |

`where`'s operators are words, not shell redirections: in `where COL > 100` the first
`<`, `>`, `<=`, `>=` after the column is the comparison (msh's parser makes that one
exception), and anything later is a real redirection, so `ps | where rss > 100 > out`
writes the rendered table to `out`.

Values: `int` and `size` columns compare numerically; a `size` literal may have a unit
(`100K`, `2M`, `1G`; bare numbers are bytes). `str` columns compare as text. `rss` in
`ps` is an `int` in KiB (like Linux `ps`), so `where rss > 100` means more than 100 KiB.

### Wire format (`rt::table`)

Line-based UTF-8, so it survives any byte pipe and is readable with `cat`:

```text
\x1emilk-table 1                      first line: ASCII RS + magic + version
pid:int<TAB>name:str<TAB>rss:int      column names and types (int, size, str)
1<TAB>init<TAB>152                    one line per row
7<TAB>msh<TAB>208
```

Fields escape `\` as `\\`, tab as `\t`, newline as `\n`. An empty field in an int column
is "no value". The renderer right-aligns numbers and shows `size` columns as `1.5K`.
A `ver` column holds version numbers and compares them semver-ish (`rt::version`:
`1.10.0 > 1.9.0`, `1.2 == 1.2.0`, `1.0.0-rc.1 < 1.0.0`), so `where version > 1.0` means it.

### Columns of the producers

* `ps`: `pid ppid pgid tid:int, state:str, cpu:int (ms), rss:int (KiB), name:str`
* `files list`: `name:str, kind:str (file folder link device pipe socket), size:size,
  mode:str (rwxr-xr-x), owner:str, modified:str`
* `trash list`: `name:str, from:str (where it was), deleted:str, days_left:int (empty:
  never), size:size, trash:str (the Trash folder it is in)`
* `env`: `name:str, value:str`
* `milk list`: `name:str, version:ver, auto:str (yes/no), description:str`
* `milk search`: `name:str, version:ver (newest), installed:ver (empty if not), status:str
  (installed / upgrade / available), description:str`

### Why this design

* No new kernel feature and no fd sniffing: negotiation is explicit, through msh, which
  already knows the whole pipeline. Programs that don't know about tables never see one.
* A text header + tab-separated rows is trivially parsed and generated by small tools
  in any language, and the magic line makes a table impossible to confuse with text.
* Typed columns (int / size / str) are what makes `where rss > 100` and `sort-by size`
  do the right thing without guessing.

### Not done (ideas)

Nested values (lists, records), `group-by`, `update`, `to-csv` / `from-csv`, tables
from more tools (`df`, `free`, `/proc/<pid>` as `ps -l`), colour in the renderer.
