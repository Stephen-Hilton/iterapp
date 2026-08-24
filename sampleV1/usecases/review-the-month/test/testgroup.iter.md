---
name: "test testgroup"
description: "testgroup for test"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# Review the month — end-to-end tests

The groups below are declared with **empty testlists**: this is the state a
freshly created use-case is born in. The journey is described, the coverage it
needs is named, and no scripts exist yet — which is exactly the gap the test
sweep turns into a testwriter authoring item, so the coverage follows without
anyone remembering to ask for it.

<!-- iterapp:testgroups
{"label":"review the month E2E","desc":"Walk the reading journey through the CLI: a month of movements reads back in the order it happened with the right total, an empty month answers normally rather than erroring, and asking twice changes nothing.","auto_fix":false,"lastrun":"","result":"","counts":"","testlist":[]}
{"label":"take it away as a file E2E","desc":"The same walk exported as comma-separated values: a header row, one row per movement, a total row, and notes containing commas or quotation marks still landing in one cell.","auto_fix":false,"lastrun":"","result":"","counts":"","testlist":[]}
-->
