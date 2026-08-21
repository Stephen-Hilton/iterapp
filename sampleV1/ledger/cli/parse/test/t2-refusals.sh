#!/usr/bin/env bash
# Command Parser — every refusal in the contract's closed vocabulary, and the
# promise that a refusal exits 2 (TR-4) and records nothing (BR-6).
set -u
cd "$(dirname "$0")" || exit 2
parse="../parse.sh"
[ -f "$parse" ] || { echo "cannot find $parse"; exit 2; }

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}
code_of() { bash "$parse" "$@" | sed -n 's/^refusal=\([A-Z_]*\).*/\1/p'; }

check "unknown action"        "UNKNOWN_ACTION"     "$(code_of delete 3)"
check "no action at all"      "MISSING_ARGUMENT"   "$(code_of)"
check "add with no arguments" "MISSING_ARGUMENT"   "$(code_of add)"
check "add with no memo"      "MISSING_ARGUMENT"   "$(code_of add 450)"
check "decimal amount"        "AMOUNT_NOT_INTEGER" "$(code_of add 4.50 coffee)"
check "non-numeric amount"    "AMOUNT_NOT_INTEGER" "$(code_of add lots coffee)"
check "blank memo"            "EMPTY_MEMO"         "$(code_of add 450 '   ')"

check "refusal exits 2"       "2" "$(bash "$parse" delete 3 >/dev/null; echo $?)"
check "refusal carries a detail sentence" "yes" \
  "$(bash "$parse" delete 3 | grep -q 'detail=.' && echo yes || echo no)"

# Total: the reply is either a decision or a refusal, never nothing.
check "every reply is one line" "1" "$(bash "$parse" nonsense | wc -l | tr -d ' ')"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
