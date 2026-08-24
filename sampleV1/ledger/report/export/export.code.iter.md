---
name: "CSV Exporter"
level: component
description: "Writes the same report as a comma-separated file a spreadsheet can open."
owner: bespoke
teststate: omit
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  []
  inputs:     []
  outputs:    []
  bizreqs:    ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Long Description

The CSV Exporter answers BR-5: the same report, but as comma-separated values so
it can be opened in a spreadsheet. It walks the log exactly the way the Report
Renderer does — same order, same running total — and differs only in how it
writes each line out.

It writes a header row naming the columns, then one row per movement, then a
final row carrying the total. Notes are wrapped in quotation marks and any
quotation mark inside a note is doubled, which is the escaping rule spreadsheets
expect.

**This object is parked out of the automated test loop** (`test_loop: omit` in
its frontmatter). Proving a CSV file really opens correctly means opening it in a
spreadsheet program, which is not something the sample can do on a bare machine
with nothing installed (TR-1). Rather than let a permanently-red testgroup sit in
the sweep and train everyone to ignore it, the object declares itself out. Its
testgroup file still exists and still describes what the tests would need to
prove; bringing it back in is `iter testloop --include export` once there is a
way to check the claim honestly.
