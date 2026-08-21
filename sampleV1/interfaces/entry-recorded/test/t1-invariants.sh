#!/usr/bin/env bash
# entry-recorded — contract enforcement against the real provider (the Entry
# Store).  An event contract's worked examples are one-directional, so what is
# worth enforcing is the invariants: seq with no gaps, balance as a running sum,
# and a notice only ever announced after the fact.
set -u
cd "$(dirname "$0")" || exit 2
store="../../../ledger/cli/store/store.sh"
[ -f "$store" ] || { echo "cannot find the provider at $store"; exit 2; }

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       contract: $2"; echo "       provider: $3"; fi
}

# The three worked examples, in contract order.
check "example 1" "seq=1 amount=125000 balance=125000 memo=paycheck for March" \
  "$(bash "$store" append 125000 'paycheck for March')"
check "example 2" "seq=2 amount=-450 balance=124550 memo=coffee" \
  "$(bash "$store" append -450 coffee)"
check "example 3" "seq=3 amount=-124550 balance=0 memo=rent" \
  "$(bash "$store" append -124550 rent)"

# Invariant — seq starts at 1 and rises by exactly one, no gaps, no repeats.
export LEDGER_FILE="$scratch/sequence.log"
seqs=""
running=0
ok_balance=yes
for amount in 10 -3 5 -12 400; do
  notice=$(bash "$store" append "$amount" "movement $amount")
  s="${notice#seq=}"; s="${s%% *}"
  b="${notice#*balance=}"; b="${b%% *}"
  seqs="$seqs$s "
  running=$((running + amount))
  [ "$b" = "$running" ] || ok_balance=no
done
check "seq rises by one with no gaps" "1 2 3 4 5 " "$seqs"
check "balance is the running sum of every amount so far" "yes" "$ok_balance"

# Invariant — only after the fact: the movement is in the log by the time its
# notice is announced, so the log's own last line must match the notice.
notice=$(bash "$store" append -7 "last one")
last=$(tail -n 1 "$LEDGER_FILE")
s="${notice#seq=}"; s="${s%% *}"
check "the notice's movement is already in the log" "$s|-7|last one" "$last"

# Invariant — no movement in the log without a notice, and none the other way:
# six appends, six lines.
check "one line per notice announced" "6" "$(wc -l <"$LEDGER_FILE" | tr -d ' ')"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
