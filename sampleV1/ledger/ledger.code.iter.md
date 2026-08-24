---
name: "Ledger System"
level: context
description: "The running program a person interacts with, and the one file it keeps its records in."
owner: bespoke
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  ["{thisfiledir}/cli/cli.code.iter.md", "{thisfiledir}/report/report.code.iter.md"]
  bizreqs:   ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/*.testgroup.iter.md"]
---

# Long Description

The Ledger System is the boundary around everything that runs. Outside it there
is exactly one person, at a terminal, typing commands. Inside it there is the
program and the single text file where records are kept. Nothing else crosses the
line: no other program calls in, and this system calls nothing out.

Drawing the boundary here is what makes the rest of the project easy to reason
about. Anything inside can be changed freely as long as two things stay true from
the outside: the person types the same commands and gets the same answers, and
the record file keeps everything ever recorded.

Inside the boundary there are two halves:

- **Ledger CLI** (`cli/`) — the half a person touches. It reads what they typed,
  works out what they meant, and keeps the records.
- **Report Renderer** (`report/`) — the half that reads records back out and
  turns them into something a person can read, either as lined-up text or as a
  spreadsheet file.

The record file itself is not code, so it has no folder of its own. Its location
travels between the parts as an environment variable named `LEDGER_FILE`, which
is what lets the tests point every part at a scratch file instead of a real one.
