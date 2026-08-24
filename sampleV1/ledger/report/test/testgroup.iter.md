---
name: "test testgroup"
description: "testgroup for test"
children:
  testpaths: ["{thisfiledir}/*.sh"]
---

# Report Renderer — test groups

The Renderer's whole job is reading, so its tests care about two things: that the
numbers it reports are the numbers in the log, and that reading really is only
reading.

<!-- iterapp:testgroups
{"label":"report rendering","desc":"Totals and the ordered report, including BR-7's empty log, and the promise that reading never changes the log.","auto_fix":true,"lastrun":"2026-08-21T22:13:52Z","result":"passed","counts":"9/9","testlist":[{"id":"t1","name":"rendering","desc":"absent, empty and populated logs all render correctly and leave the file untouched","shell":"t1-render.sh"}]}
-->
