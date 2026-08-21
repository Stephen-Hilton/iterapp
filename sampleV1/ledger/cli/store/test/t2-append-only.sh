#!/usr/bin/env bash
# Entry Store — BR-2: nothing already recorded is ever silently changed or
# dropped.  The check is byte-level: whatever the log held before a new movement
# must still be there, unchanged, as its leading bytes.
set -u
cd "$(dirname "$0")" || exit 2
store="../store.sh"
[ -f "$store" ] || { echo "cannot find $store"; exit 2; }

scratch=$(mktemp -d) || exit 2
trap 'rm -rf "$scratch"' EXIT
export LEDGER_FILE="$scratch/ledger.log"

pass=0; fail=0
check() {
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"
  else fail=$((fail + 1)); echo "FAIL $1"; echo "       expected: $2"; echo "       actual:   $3"; fi
}

bash "$store" append 100 first >/dev/null
bash "$store" append 200 second >/dev/null
before=$(cat "$LEDGER_FILE")
bytes=${#before}

bash "$store" append 300 third >/dev/null
after=$(cat "$LEDGER_FILE")

check "earlier lines survive verbatim" "$before" "${after:0:$bytes}"
check "the file only grew"             "yes"     "$([ ${#after} -gt "$bytes" ] && echo yes || echo no)"
check "no earlier position was reused" "1 2 3"   "$(cut -d'|' -f1 <"$LEDGER_FILE" | tr '\n' ' ' | sed 's/ $//')"

# The store creates the log's directory rather than failing on a fresh machine.
export LEDGER_FILE="$scratch/nested/deeper/ledger.log"
bash "$store" append 42 "on a fresh machine" >/dev/null
check "an absent log is created, not an error" "yes" \
  "$([ -f "$LEDGER_FILE" ] && echo yes || echo no)"

# A malformed request is refused with exit 2 and writes nothing.
export LEDGER_FILE="$scratch/refusal.log"
bash "$store" append 42 >/dev/null 2>&1
check "append without a memo exits 2" "2" \
  "$(bash "$store" append 42 >/dev/null 2>&1; echo $?)"
check "a refused append records nothing" "no" \
  "$([ -f "$LEDGER_FILE" ] && echo yes || echo no)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
