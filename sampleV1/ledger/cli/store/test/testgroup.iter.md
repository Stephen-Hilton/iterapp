---
name: "test testgroup"
description: "testgroup for test"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# Entry Store — test groups

The Store is the only part of the project that writes, which makes it the only
place BR-2 can be broken. Its tests are correspondingly blunt: append some
movements, then read the raw bytes back and prove the earlier ones are still
exactly where they were.

<!-- iterapp:testgroups
{"label":"store append","desc":"Appending a movement: the position it takes, the running balance, and the entry-recorded notice announced after the write.","auto_fix":true,"lastrun":"2026-08-21T22:15:22Z","result":"passed","counts":"8/8","testlist":[{"id":"t1","name":"append and announce","desc":"seq starts at 1 and rises by one, balance accumulates, and every append announces a contract-shaped notice","shell":"t1-append.sh"}]}
{"label":"store append-only","desc":"BR-2 held byte for byte: earlier lines survive verbatim, positions are never reused, and a refused append leaves no file behind.","auto_fix":false,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"6/6","testlist":[{"id":"t2","name":"append-only","desc":"the log only ever grows; a refused request writes nothing at all","shell":"t2-append-only.sh"}]}
-->
