#!/bin/sh
# test group: basic greeting — default (no-name) behavior
cd "$(dirname "$0")/.."
pass=0; fail=0

out=$(./src/greet.sh)
[ "$out" = "Hello, World!" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: default greeting, got: $out" >&2; }

out=$(./src/greet.sh; echo x); out="${out%x}"
[ "$out" = "Hello, World!
" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: exactly one line expected" >&2; }

total=$((pass+fail))
if [ "$fail" -eq 0 ]; then echo "passed $pass/$total"; exit 0; else echo "failed $pass/$total"; exit 1; fi
