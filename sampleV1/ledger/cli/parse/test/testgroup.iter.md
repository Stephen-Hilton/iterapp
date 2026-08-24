---
name: "test testgroup"
description: "testgroup for test"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# Command Parser — test groups

The Parser is where every judgment about a typed request lives, so its tests are
the project's first line of defence: if a bad request gets past here, the parts
downstream have no checking of their own to catch it.

Two groups, split by which half of the `ledger-command` contract they exercise —
the decisions it reaches, and the refusals it returns instead.

<!-- iterapp:testgroups
{"label":"parser decisions","desc":"The three commands the parser can decide on, their exact reply line, and that deciding is deterministic and free of side effects.","auto_fix":true,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"8/8","testlist":[{"id":"t1","name":"decisions","desc":"add/total/report produce the contract's success reply and exit 0","shell":"t1-decisions.sh"}]}
{"label":"parser refusals","desc":"Every code in the contract's closed refusal vocabulary, each reachable, each exiting 2 with a detail sentence.","auto_fix":true,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"10/10","testlist":[{"id":"t2","name":"refusals","desc":"UNKNOWN_ACTION, MISSING_ARGUMENT, AMOUNT_NOT_INTEGER and EMPTY_MEMO all reachable and all exit 2","shell":"t2-refusals.sh"}]}
-->
