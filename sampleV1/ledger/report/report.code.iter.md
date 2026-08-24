---
name: "Report Renderer"
level: container
description: "Reads the log back out and turns it into something a person can read."
owner: bespoke
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  ["{thisfiledir}/export/export.code.iter.md"]
  inputs:     ["{topdir}/interfaces/entry-recorded/entry-recorded.interface.iter.md"]
  bizreqs:   ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Long Description

The Report Renderer is the reading half of the system. It never writes anything
and never decides anything; it opens the log, walks it from the top, and formats
what it finds.

It produces two answers. The **total** is a single number: what the whole log
adds up to, in cents. The **report** is every movement in the order it was
recorded — position, amount, note — followed by that same total on the last line.
BR-7 says asking either question of an empty log is normal, not an error, so an
empty log gives an empty list and a total of zero, and exits successfully.

Underneath it sits the CSV Exporter (`export/`), which formats the same walk of
the same log as comma-separated values for a spreadsheet.

The Renderer reads the log directly rather than replaying `entry-recorded`
notices, because it runs as a fresh process each time and there is no notice
history to replay. It still declares that it consumes `entry-recorded`: the shape
of a movement it expects to find in the log is exactly the shape that contract
describes, so a change to the contract is a change to this object.

## entry-recorded binding

The Renderer reconstructs `entry-recorded` notices by reading the log file named
by `LEDGER_FILE`, one line per movement, and computing the running `balance`
itself as it walks. This is why the contract's invariant that balance is the sum
of all prior amounts matters here: it is the rule the Renderer implements, and
the reason its total and the Entry Store's announced total can never disagree.
