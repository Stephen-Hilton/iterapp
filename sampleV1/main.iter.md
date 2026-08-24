---
projectname: "Sample Ledger"
projectdescription: "A tiny personal money log: record what came in and what went out, then ask what it adds up to."
globalscandirs: ["{topdir}/"]
globalinterfacedir: "{topdir}/interfaces/"
globalusecasedir: "{topdir}/usecases/"
globalcontextfiles: ["{topdir}/reqs/bizreq.iter.md", "{topdir}/reqs/techreq.iter.md"]
children:
  codenodes: ["{topdir}/ledger/ledger.code.iter.md"]
---

# Long Description

Sample Ledger is the whole of this project: a small program a person runs from a
terminal to keep track of money moving in and out, and to ask what it all adds up
to. It exists so that iterapp itself has something honest to work on — a project
small enough to read in ten minutes, but with enough moving parts that every
piece of the engine has something real to grip.

A person uses it by typing a command. There are three things they can ask for:
record a movement of money, print the running total, or print a report of
everything so far. Nothing runs in the background, nothing listens on a network,
and nothing is stored anywhere except one plain text file on the same machine.

The project is laid out as a nest of parts, each one a folder that owns its own
code and its own tests:

- `ledger/` — the Ledger System, everything that makes the program work. See
  `ledger/ledger.marker.iter.md`.
  - `ledger/cli/` — the Ledger CLI, the piece a person actually types at.
    - `ledger/cli/parse/` — the Command Parser, which works out what the person
      asked for.
    - `ledger/cli/store/` — the Entry Store, which keeps the records.
  - `ledger/report/` — the Report Renderer, which turns records into something
    readable.
    - `ledger/report/export/` — the CSV Exporter, which writes the same report as
      a spreadsheet file.

Two agreements between those parts are written down separately, in
`interfaces/`: the shape of a parsed command, and the shape of the notice sent
out when a record is stored. Two journeys a person actually takes are written
down in `usecases/`.

The requirements every part must honor are in `reqs/bizreq.iter.md` (what the
person needs) and `reqs/techreq.iter.md` (how it must be built).
