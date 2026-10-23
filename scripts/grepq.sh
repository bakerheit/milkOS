# Sourced by the test scripts (after their `cd` to the repo root): a `grep` that always reads all
# of its input.
#
# `echo "$L" | grep -q PATTERN` stops reading at the first match, and so does `grep -m N` after
# N. When the writer still has output to send (a long log, or a slow writer on a loaded
# machine), it then dies of SIGPIPE, and `set -o pipefail` turns a pipeline whose grep matched
# into a failure: a check that failed although the line was there. This grep drops -q
# (--quiet, --silent) and sends the output to /dev/null instead, and does -m N by keeping the
# first N lines of the output; either way it reads to the end, and the exit status is grep's
# own. Everything else goes to the real grep unchanged.
grep() {
  local a=() q=0 m="" opts=1 x
  while [ $# -gt 0 ]; do
    x="$1"; shift
    if [ "$opts" = 1 ]; then
      case "$x" in
        --) opts=0 ;;
        -m|--max-count) m="${1:-}"; shift; continue ;;
        --max-count=*) m="${x#--max-count=}"; continue ;;
        -m[0-9]*) m="${x#-m}"; continue ;;
        -e|-f|-A|-B|-C|--regexp|--file|--context)
          a+=("$x"); [ $# -gt 0 ] && { a+=("$1"); shift; }; continue ;;
        --quiet|--silent) q=1; continue ;;
        --*) ;;
        -*q*) q=1; x="${x//q/}"; [ "$x" = - ] && continue ;;
        -*) ;;
        *) opts=0 ;;
      esac
    fi
    a+=("$x")
  done
  if [ "$q" = 1 ]; then
    command grep ${a[@]+"${a[@]}"} >/dev/null
  elif [ -n "$m" ]; then
    command grep ${a[@]+"${a[@]}"} | awk -v n="$m" 'NR <= n'
    return "${PIPESTATUS[0]}"
  else
    command grep ${a[@]+"${a[@]}"}
  fi
}
