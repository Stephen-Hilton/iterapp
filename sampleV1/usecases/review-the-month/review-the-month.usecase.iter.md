---
name: "Review the month"
description: "Someone looks back over everything they recorded, then takes it away as a spreadsheet."
teststate: inherit
children:
  codenodes:  ["{topdir}/ledger/cli/cli.code.iter.md", "{topdir}/ledger/cli/parse/parse.code.iter.md", "{topdir}/ledger/report/report.code.iter.md", "{topdir}/ledger/report/export/export.code.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Review the month

It is the end of the month. Someone who has been writing down coffees and
paycheques for four weeks now wants to see the whole picture: everything they
recorded, in the order it happened, and what it comes to. Then they want to take
that away as a file they can open in a spreadsheet and share with someone else.

This is the reading journey, and it touches an almost entirely different half of
the project from the writing one. Nothing here changes anything.

The person types the request for a report. The **Ledger CLI** (`ledger/cli`)
hands the words to the **Command Parser** (`ledger/cli/parse`) as always, which
decides this is a `report` and needs no further arguments.

The CLI passes that to the **Report Renderer** (`ledger/report`). The Renderer
opens the log and walks it from the top, printing each movement — its position,
its amount, its note — in the order it was recorded, keeping a running total as
it goes. The last line is that total. It never writes, so a person can ask for a
report as many times as they like without any risk to what they have recorded.

If the person has recorded nothing at all, this is still a normal request with a
normal answer: an empty list and a total of zero. Nobody is told off for asking
about an empty month.

Then they want it as a file. The **CSV Exporter** (`ledger/report/export`) walks
the same log the same way, and differs only in shape: a header row naming the
columns, one row per movement, and a final row carrying the total, with notes
quoted the way spreadsheets expect.

That last step is the one part of this journey the automated tests do not check.
Proving a file really opens in a spreadsheet means having a spreadsheet, and this
project deliberately runs on a machine with nothing installed. The Exporter is
parked out of the test loop for exactly that reason, and says so on its marker.
