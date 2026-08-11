#!/bin/sh
# test group: contract — exit codes and stdout discipline (TR-3, TR-4)
cd "$(dirname "$0")/.."
pass=0; fail=0

./src/greet.sh >/dev/null 2>&1
[ $? -eq 0 ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: exit 0 expected on success" >&2; }

err=$(./src/greet.sh Ada 2>&1 >/dev/null)
[ -z "$err" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: stderr must be empty on success, got: $err" >&2; }

total=$((pass+fail))
if [ "$fail" -eq 0 ]; then echo "passed $pass/$total"; exit 0; else echo "failed $pass/$total"; exit 1; fi
