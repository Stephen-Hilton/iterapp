#!/usr/bin/env bash
# Entry Store — the only writer of the log.  Serves `entry-recorded`.
#
#   store.sh append <amount-cents> <memo>   append one movement, announce it
#   store.sh next-seq                       what position the next movement takes
#   store.sh balance                        what the whole log adds up to, in cents
#
# The log lives at $LEDGER_FILE (TR-5), one movement per line:
#   <seq>|<amount-cents>|<memo>
set -u

ledger="${LEDGER_FILE:-}"
if [ -z "$ledger" ]; then
  echo "store: LEDGER_FILE is not set" >&2
  exit 2
fi

# An absent log is an empty log, never an error (BR-7).
next_seq() {
  [ -f "$ledger" ] || { echo 1; return; }
  awk -F'|' 'NF { last = $1 } END { print last + 1 }' "$ledger"
}

balance() {
  [ -f "$ledger" ] || { echo 0; return; }
  awk -F'|' 'NF { sum += $2 } END { print sum + 0 }' "$ledger"
}

case "${1:-}" in
  next-seq) next_seq ;;
  balance)  balance ;;
  append)
    shift
    if [ "$#" -lt 2 ]; then
      echo "store: append needs an amount and a memo" >&2
      exit 2
    fi
    amount="$1"
    memo="$2"
    seq=$(next_seq)
    dir=$(dirname "$ledger")
    [ -d "$dir" ] || mkdir -p "$dir"
    # Append only.  Nothing in this file ever rewrites or truncates the log.
    printf '%s|%s|%s\n' "$seq" "$amount" "$memo" >>"$ledger"
    # Announce strictly AFTER the write: a notice always describes something
    # that really happened.
    printf 'seq=%s amount=%s balance=%s memo=%s\n' "$seq" "$amount" "$(balance)" "$memo"
    ;;
  *)
    echo "store: unknown request '${1:-}'" >&2
    exit 2
    ;;
esac
exit 0
