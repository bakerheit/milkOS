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

`where` only ever prints the current folder, in a pipeline too, and takes no arguments:
`where cpu > 5` says `where takes no arguments; to filter a table use filter`. Filtering
a table is `filter` (`processes list | filter memory > 1M`, below).

## Structured pipes

Plain text pipes stay exactly as they are. On top of that, some commands can pass a
**table** (rows of named, typed columns) down a pipe, and a few shell built-ins work on
tables, Nushell style:

```sh
processes list | filter memory > 1M | sort-by name    # filter + sort, an aligned table
processes list | filter name == msh | select pid memory
files list /Commands | filter size > 100K | sort-by size -r | first 3
processes list | sort-by cpu_ms -r | first 1 | get name  # -> plain text: one value a line
processes list | filter state == running | count
services list | filter state != running | select name detail
users list | filter admin == yes | get name
system memory | sort-by size -r | first 3
files list / | to-json
cat /proc/threads | filter PRIO > 1          # plain text with a header row works too
processes list | grep msh                    # a text tool after it: it prints text
```

### Rules

1. **Producers** (`processes list`, `services list`, `users list`, `system memory`,
   `files list`, `trash list`, `env`, ...) print normal text, unless the environment variable
   `MILK_TABLE=1` is set, then they print a table. They never guess from the fd.
2. **msh decides**, per pipeline stage, from the command names:
   * a stage whose next stage is a table command gets `MILK_TABLE=1`;
   * the table commands always read and write tables;
   * wherever a table would flow into something that isn't a table command (a text
     tool, or the end of the pipeline: the terminal or a `>` file), msh inserts an
     implicit `table` stage that renders it as aligned text.
   So `processes list | grep msh` is a plain text pipe, and `processes list | filter ... |
   grep x` renders the
   table to text before `grep` sees it.
3. **Table commands** given text instead of a table treat the first line as a header and
   split every line on runs of spaces (numbers become ints). So any columnar text works:
   `/proc/threads`, `df`-style output, a `processes list` run without msh.

### Table commands (msh built-ins, run as pipeline stages)

| command | does |
|---|---|
| `filter COL OP VALUE` | keep rows; OP is `== != < <= > >=`, `=~` (contains), `!~` |
| `sort-by COL [-r]` | stable sort, numeric for int/size columns |
| `select COL...` | keep these columns, in this order |
| `first [N]`, `last [N]` | first / last N rows (default 1) |
| `get COL` | the column's values, one per line (text) |
| `count` | number of rows (text) |
| `table` | render as aligned text (what msh inserts automatically) |
| `to-json` | rows as a JSON array of objects (text) |

`filter`'s operators are words, not shell redirections: in `filter COL > 100` the first
`<`, `>`, `<=`, `>=` after the column is the comparison (msh's parser makes that one
exception), and anything later is a real redirection, so `processes list | filter memory > 1M > out`
writes the rendered table to `out`.

Values: `int` and `size` columns compare numerically; a `size` literal may have a unit
(`100K`, `2M`, `1G`; bare numbers are bytes). `str` columns compare as text. `memory` in
`processes list` is a `size`, so `filter memory > 1M` means more than a megabyte.

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
`1.10.0 > 1.9.0`, `1.2 == 1.2.0`, `1.0.0-rc.1 < 1.0.0`), so `filter version > 1.0` means it.

### Columns of the producers

* `processes list`: `pid parent group thread:int, state:str (running sleeping zombie),
  core:int (the CPU), cpu_ms:int (CPU time), memory:size, name:str` (`--kernel`: the
  kernel's threads too, with no pid, parent, group or memory)
* `services list` / `services status`: `name state:str, pid restarts uptime:int (seconds),
  user detail description:str`; `services logs`: `time:int (Unix), boot:str, seq:int,
  name:str, pid uid:int, level:str, message:str`
* `users list`: `name:str, id:int, group groups admin (yes/no) home shell full_name:str`
* `system memory`: `group:str (Activity Monitor's: Apps, Kernel heap, ...), size:size,
  percent:str`
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
* Typed columns (int / size / str) are what makes `filter rss > 100` and `sort-by size`
  do the right thing without guessing.

### Not done (ideas)

Nested values (lists, records), `group-by`, `update`, `to-csv` / `from-csv`, tables
from more tools (`df`, `processes info` as a row), colour in the renderer.
