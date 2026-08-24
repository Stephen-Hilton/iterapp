---
name: "test testgroup"
description: "testgroup for test"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# Ledger CLI — test groups

The CLI holds no rules, so its tests do not re-check them. They check the two
things only the front door can get wrong: reaching the part that owns the rule,
and handing back what that part said without mangling it.

<!-- iterapp:testgroups
{"label":"cli wiring","desc":"Each action reaches the part that owns it and the answer comes back intact, including from an unrelated working directory.","auto_fix":true,"lastrun":"2026-08-21T22:15:22Z","result":"passed","counts":"8/8","testlist":[{"id":"t1","name":"wiring","desc":"add, total and report route correctly; the confirmation names amount, memo and new total","shell":"t1-wiring.sh"}]}
{"label":"cli refusals","desc":"BR-6 end to end: a refused request exits 2, says one plain sentence on stderr, prints nothing on stdout, and leaves the log byte-identical.","auto_fix":false,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"14/14","testlist":[{"id":"t2","name":"refusal passthrough","desc":"four refusal shapes, each leaving the log untouched","shell":"t2-refusal-passthrough.sh"}]}
-->
