#!/usr/bin/env bash
# Ledger CLI — BR-6 end to end: a request the system does not understand gets
# one plain sentence on stderr, exit 2, and leaves the log untouched.
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

bash "$cli" add 1000 groundwork >/dev/null
before=$(cat "$LEDGER_FILE")

for bad in "delete 3" "add 4.50 coffee" "add 450" ""; do
  # shellcheck disable=SC2086
  out=$(bash "$cli" $bad 2>&1 >/dev/null)
  code=$(bash "$cli" $bad >/dev/null 2>&1; echo $?)
  label="${bad:-(no arguments)}"
  check "refused '$label' exits 2" "2" "$code"
  check "refused '$label' says one sentence on stderr" "1" "$(printf '%s' "$out" | wc -l | tr -d ' ' | sed 's/^0$/1/')"
  check "refused '$label' says it plainly" "yes" \
    "$(printf '%s' "$out" | grep -q '^ledger: .' && echo yes || echo no)"
done

check "nothing was recorded by any refusal" "$before" "$(cat "$LEDGER_FILE")"

# TR-3: the refusal sentence goes to stderr, never stdout — a caller parsing
# stdout must see nothing at all.
check "a refusal prints nothing on stdout" "" "$(bash "$cli" delete 3 2>/dev/null)"

echo "ITER_RESULT pass=$pass fail=$fail total=$((pass + fail))"
[ "$fail" -eq 0 ] || exit 1
exit 0
