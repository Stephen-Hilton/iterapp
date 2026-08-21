#!/usr/bin/env bash
# Record a movement — the end-to-end journey, walked the way a person walks it:
# through the CLI, with nothing reached into directly.
set -u
cd "$(dirname "$0")" || exit 2
cli="../../../ledger/cli/ledger.sh"
[ -f "$cli" ] || { echo "cannot find the CLI at $cli"; exit 2; }

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

# The journey: someone buys a coffee and writes it down.
check "the coffee is written down, with the new total" \
  "recorded -450  coffee  (total -450)" \
  "$(bash "$cli" add -450 coffee)"

check "and asking for the total agrees" "-450" "$(bash "$cli" total)"

# The same journey again, from a starting balance.
bash "$cli" add 125000 "paycheck for March" >/dev/null
check "a second movement carries the total forward" \
  "recorded -1200  lunch with Dana  (total 123350)" \
  "$(bash "$cli" add -1200 lunch with Dana)"

# The refusal fork of the same journey: nothing is left behind.
before=$(cat "$LEDGER_FILE")
bash "$cli" add 4.50 coffee >/dev/null 2>&1
check "a refused movement records nothing" "$before" "$(cat "$LEDGER_FILE")"
check "and the total is unchanged" "123350" "$(bash "$cli" total)"

# Everything the person recorded is still there, in the order they did it.
check "the log reads back in the order it happened" \
  "1  -450  coffee
2  125000  paycheck for March
3  -1200  lunch with Dana
TOTAL 123350" \
  "$(bash "$cli" report)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
