#!/usr/bin/env bash
# Ledger CLI — the front door routes to the right part and hands back the right
# answer.  This is a wiring test: it does not re-check the rules, only that the
# CLI reached the part that owns them.
set -u
cd "$(dirname "$0")" || exit 2
cli="../ledger.sh"
[ -f "$cli" ] || { echo "cannot find $cli"; exit 2; }

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

check "empty log: total is zero, not an error" "0" "$(bash "$cli" total)"
check "empty log: total exits 0"               "0" "$(bash "$cli" total >/dev/null; echo $?)"
check "empty log: report is just the total"    "TOTAL 0" "$(bash "$cli" report)"

# CLI-BIZ-003: the confirmation names the amount, the note and the new total.
check "add confirms with amount, memo and new total" \
  "recorded 125000  paycheck for March  (total 125000)" \
  "$(bash "$cli" add 125000 paycheck for March)"

check "a second add carries the running total forward" \
  "recorded -450  coffee  (total 124550)" \
  "$(bash "$cli" add -450 coffee)"

check "total after two movements" "124550" "$(bash "$cli" total)"

check "report lists movements in order, then the total" \
  "1  125000  paycheck for March
2  -450  coffee
TOTAL 124550" \
  "$(bash "$cli" report)"

# CLI-TECH-002: the CLI finds its parts by its own location, so it works from
# anywhere the person happens to be standing.
check "works from an unrelated working directory" "124550" \
  "$(cd "$scratch" && bash "$OLDPWD/$cli" total)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
