#!/usr/bin/env bash
# Ledger CLI — the front door.  Holds no rules of its own (CLI-TECH-001): it
# asks the Command Parser what was meant, then hands the work to the Entry Store
# or the Report Renderer.
#
#   ledger.sh add <amount-cents> <memo...>
#   ledger.sh total
#   ledger.sh report
set -u

# Locate the parts relative to THIS file, never the caller's directory
# (CLI-TECH-002).
here=$(cd "$(dirname "$0")" && pwd)
parse="$here/parse/parse.sh"
store="$here/store/store.sh"
report="$here/../report/report.sh"

export LEDGER_FILE="${LEDGER_FILE:-$here/ledger.log}"

# Ask what the person meant.  A refusal ends the run with nothing recorded
# (BR-6); the parser's exit code passes straight through (CLI-TECH-003).
decision=$(bash "$parse" "$@")
status=$?
if [ "$status" -ne 0 ]; then
  detail="${decision#*detail=}"
  echo "ledger: $detail" >&2
  exit "$status"
fi

action="${decision#action=}"
action="${action%% *}"

case "$action" in
  add)
    rest="${decision#*amount=}"
    amount="${rest%% *}"
    memo="${decision#*memo=}"
    notice=$(bash "$store" append "$amount" "$memo") || exit $?
    # Confirm with amount, note and the new running total, so the person never
    # has to ask for the total just to check the record landed (CLI-BIZ-003).
    balance="${notice#*balance=}"
    balance="${balance%% *}"
    echo "recorded $amount  $memo  (total $balance)"
    ;;
  total)
    bash "$report" total
    ;;
  report)
    bash "$report" report
    ;;
esac
exit 0
