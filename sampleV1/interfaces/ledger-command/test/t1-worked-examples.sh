#!/usr/bin/env bash
# ledger-command — contract enforcement.  Every worked example in the contract,
# replayed against the real provider (the Command Parser).  Interfaces are
# enforcement, not documentation: when the code drifts from the contract, this
# turns red rather than the two quietly disagreeing.
set -u
cd "$(dirname "$0")" || exit 2
parse="../../../ledger/cli/parse/parse.sh"
[ -f "$parse" ] || { echo "cannot find the provider at $parse"; exit 2; }

pass=0; fail=0
example() { # example <expected-reply-line> <argv...>
  expected="$1"; shift
  actual=$(bash "$parse" "$@")
  if [ "$expected" = "$actual" ]; then
    pass=$((pass + 1)); echo "ok   argv=[$*]"
  else
    fail=$((fail + 1)); echo "FAIL argv=[$*]"
    echo "       contract: $expected"
    echo "       provider: $actual"
  fi
}

# The eight worked examples, in contract order.
example "action=add amount=-450 memo=coffee"                     add -450 coffee
example "action=add amount=125000 memo=paycheck for March"       add 125000 paycheck for March
example "action=total"                                           total
example "action=report"                                          report
example "refusal=UNKNOWN_ACTION detail=no action named 'delete'" delete 3
example "refusal=MISSING_ARGUMENT detail=add needs an amount and a memo" add 450
example "refusal=AMOUNT_NOT_INTEGER detail=amount must be whole cents"   add 4.50 coffee
example "refusal=MISSING_ARGUMENT detail=no action given"

# Invariant — the refusal vocabulary is closed.  Anything the provider refuses
# must name one of the four declared codes and no other.
for bad in "delete 3" "add" "add 450" "add 4.50 c" "add 450 '  '" "" "wobble"; do
  # shellcheck disable=SC2086
  code=$(bash "$parse" $bad 2>/dev/null | sed -n 's/^refusal=\([A-Z_]*\).*/\1/p')
  [ -n "$code" ] || continue
  case "$code" in
    UNKNOWN_ACTION|MISSING_ARGUMENT|AMOUNT_NOT_INTEGER|EMPTY_MEMO)
      pass=$((pass + 1)); echo "ok   refusal code '$code' is in the vocabulary" ;;
    *)
      fail=$((fail + 1)); echo "FAIL refusal code '$code' is not in the contract's vocabulary" ;;
  esac
done

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
