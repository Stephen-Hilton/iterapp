#!/bin/sh
# test group: contract — exit codes and stdout discipline (TR-3, TR-4)
cd "$(dirname "$0")/.."
pass=0; fail=0

./src/greet.sh >/dev/null 2>&1
[ $? -eq 0 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: exit 0 expected on success" >&2; }

err=$(./src/greet.sh Ada 2>&1 >/dev/null)
[ -z "$err" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: stderr must be empty on success, got: $err" >&2; }

total=$((pass+fail))
echo "ITER_RESULT pass=$pass fail=$fail total=$total"
if [ "$fail" -eq 0 ]; then exit 0; else exit 1; fi
