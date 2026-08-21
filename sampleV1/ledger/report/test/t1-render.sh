#!/usr/bin/env bash
# Report Renderer — reading the log back out, including the empty-log case BR-7
# calls normal rather than an error.
set -u
cd "$(dirname "$0")" || exit 2
report="../report.sh"
[ -f "$report" ] || { echo "cannot find $report"; exit 2; }

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

# BR-7: an absent log and an empty log both answer normally.
check "absent log: total is 0"          "0"       "$(bash "$report" total)"
check "absent log: total exits 0"       "0"       "$(bash "$report" total >/dev/null; echo $?)"
check "absent log: report is TOTAL 0"   "TOTAL 0" "$(bash "$report" report)"
: >"$LEDGER_FILE"
check "empty log: total is 0"           "0"       "$(bash "$report" total)"
check "empty log: report is TOTAL 0"    "TOTAL 0" "$(bash "$report" report)"

printf '1|125000|paycheck for March\n2|-450|coffee\n3|-124550|rent\n' >"$LEDGER_FILE"

check "total sums every movement" "0" "$(bash "$report" total)"
check "report lists movements in recorded order, then the total" \
  "1  125000  paycheck for March
2  -450  coffee
3  -124550  rent
TOTAL 0" \
  "$(bash "$report" report)"

# The Renderer never writes: the log must be untouched after reading it.
before=$(cat "$LEDGER_FILE")
bash "$report" report >/dev/null
bash "$report" total >/dev/null
check "reading never changes the log" "$before" "$(cat "$LEDGER_FILE")"

check "an unknown request exits 2" "2" "$(bash "$report" sideways >/dev/null 2>&1; echo $?)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
