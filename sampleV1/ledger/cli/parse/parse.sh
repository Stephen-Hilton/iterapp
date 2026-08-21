#!/usr/bin/env bash
# Command Parser — serves the `ledger-command` contract over argv/stdout.
#
# Reply on stdout, one line of key=value pairs, memo last (so it needs no
# quoting).  Exit 0 = decided, exit 2 = refused (TR-4).
set -u

refuse() {
  printf 'refusal=%s detail=%s\n' "$1" "$2"
  exit 2
}

[ "$#" -ge 1 ] || refuse MISSING_ARGUMENT "no action given"

action="$1"
shift

case "$action" in
  total|report)
    printf 'action=%s\n' "$action"
    ;;
  add)
    [ "$#" -ge 2 ] || refuse MISSING_ARGUMENT "add needs an amount and a memo"
    amount="$1"
    shift
    # Whole cents only: an optional sign then digits, nothing else (TR-6).
    case "$amount" in
      -[0-9]*|[0-9]*) ;;
      *) refuse AMOUNT_NOT_INTEGER "amount must be whole cents" ;;
    esac
    case "${amount#-}" in
      *[!0-9]*|'') refuse AMOUNT_NOT_INTEGER "amount must be whole cents" ;;
    esac
    memo="$*"
    # Collapse the memo to a single line and trim it; an empty note is refused.
    memo=$(printf '%s' "$memo" | tr '\n' ' ')
    memo="${memo#"${memo%%[![:space:]]*}"}"
    memo="${memo%"${memo##*[![:space:]]}"}"
    [ -n "$memo" ] || refuse EMPTY_MEMO "add needs a memo describing the movement"
    printf 'action=add amount=%s memo=%s\n' "$amount" "$memo"
    ;;
  *)
    refuse UNKNOWN_ACTION "no action named '$action'"
    ;;
esac
exit 0
