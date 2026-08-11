#!/bin/sh
# test group: named greeting — BR-1, BR-3
cd "$(dirname "$0")/.."
pass=0; fail=0

out=$(./src/greet.sh Ada)
[ "$out" = "Hello, Ada!" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: named greeting, got: $out" >&2; }

out=$(./src/greet.sh "Grace Hopper")
[ "$out" = "Hello, Grace Hopper!" ] && pass=$((pass+1)) || { fail=$((fail+1)); echo "FAIL: multi-word name, got: $out" >&2; }

case "$(./src/greet.sh Ada)" in
  *!) pass=$((pass+1));;
  *) fail=$((fail+1)); echo "FAIL: greeting must end with !" >&2;;
esac

total=$((pass+fail))
if [ "$fail" -eq 0 ]; then echo "passed $pass/$total"; exit 0; else echo "failed $pass/$total"; exit 1; fi
