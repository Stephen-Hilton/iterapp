#!/usr/bin/env bash
# Entry Store — appending a movement, the position it takes, and the notice it
# announces afterwards.
set -u
cd "$(dirname "$0")" || exit 2
store="../store.sh"
[ -f "$store" ] || { echo "cannot find $store"; exit 2; }

# Self-contained scratch log (TR-8): our own file, removed on the way out.
scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

check "empty log: next position is 1" "1" "$(bash "$store" next-seq)"
check "empty log: balance is 0"       "0" "$(bash "$store" balance)"

check "first notice" \
  "seq=1 amount=125000 balance=125000 memo=paycheck for March" \
  "$(bash "$store" append 125000 'paycheck for March')"

check "second notice: seq rises by one, balance accumulates" \
  "seq=2 amount=-450 balance=124550 memo=coffee" \
  "$(bash "$store" append -450 coffee)"

check "third notice: balance can return to zero" \
  "seq=3 amount=-124550 balance=0 memo=rent" \
  "$(bash "$store" append -124550 rent)"

check "next position after three movements" "4" "$(bash "$store" next-seq)"
check "balance agrees with the last notice"  "0" "$(bash "$store" balance)"
check "the log holds one line per movement"  "3" "$(wc -l <"$LEDGER_FILE" | tr -d ' ')"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
