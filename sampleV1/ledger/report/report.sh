#!/usr/bin/env bash
# Report Renderer — reads the log back out.  Never writes, never decides.
#
#   report.sh total     one number: what the log adds up to, in cents
#   report.sh report    every movement in order, then that same total
#
# An empty (or absent) log is a normal answer, not an error (BR-7).
set -u

ledger="${LEDGER_FILE:-}"
if [ -z "$ledger" ]; then
  echo "report: LEDGER_FILE is not set" >&2
  exit 2
fi

case "${1:-}" in
  total)
    if [ -f "$ledger" ]; then
      awk -F'|' 'NF { sum += $2 } END { print sum + 0 }' "$ledger"
    else
      echo 0
    fi
    ;;
  report)
    if [ -f "$ledger" ]; then
      awk -F'|' 'NF { sum += $2; printf "%s  %s  %s\n", $1, $2, $3 } END { print "TOTAL " sum + 0 }' "$ledger"
    else
      echo "TOTAL 0"
    fi
    ;;
  *)
    echo "report: unknown request '${1:-}'" >&2
    exit 2
    ;;
esac
exit 0
