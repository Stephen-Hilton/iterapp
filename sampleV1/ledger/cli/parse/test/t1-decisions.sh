#!/usr/bin/env bash
# Command Parser — the three decisions it can reach.
# Test contract (TR-7): exit 0 all held, 1 something did not, else broken.
set -u
cd "$(dirname "$0")" || exit 2
parse="../parse.sh"
[ -f "$parse" ] || { echo "cannot find $parse"; exit 2; }

pass=0; fail=0
check() { # check <what> <expected> <actual>
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

check "add, negative amount" \
  "action=add amount=-450 memo=coffee" \
  "$(bash "$parse" add -450 coffee)"

check "add, positive amount and multi-word memo" \
  "action=add amount=125000 memo=paycheck for March" \
  "$(bash "$parse" add 125000 paycheck for March)"

check "add exits 0" "0" "$(bash "$parse" add -1 x >/dev/null; echo $?)"

check "total" "action=total" "$(bash "$parse" total)"
check "total exits 0" "0" "$(bash "$parse" total >/dev/null; echo $?)"

check "report" "action=report" "$(bash "$parse" report)"
check "report exits 0" "0" "$(bash "$parse" report >/dev/null; echo $?)"

# Deciding is free of side effects: the parser cannot write, so running it a
# second time must give a byte-identical answer.
check "deterministic" "$(bash "$parse" add -450 coffee)" "$(bash "$parse" add -450 coffee)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
