---
name: "Ledger CLI"
level: container
description: "The command a person types: it works out what was asked for and makes it happen."
uses: [ledger-command, entry-recorded]
bizreq: cli.bizreq.iter.md
techreq: cli.techreq.iter.md
testgroup: test/testgroup.iter.md
test_dir: test
---

# Long Description

The Ledger CLI is the one thing a person actually runs. Everything else in the
project is something the CLI leans on. It is a single shell script, `ledger.sh`,
and its whole job is to be the front door: take the words someone typed, find out
what they meant, do it, and print the answer.

It does almost none of that work itself. It hands the typed words to the Command
Parser (`parse/`), which decides what was meant. If the Parser refuses, the CLI
prints the refusal and stops — nothing is recorded, which is what BR-6 asks for.
If the Parser decides on a movement of money, the CLI hands it to the Entry Store
(`store/`) to be written down. If the Parser decides on a total or a report, the
CLI asks the Report Renderer (`../report/`) for the answer.

Keeping the front door this thin is deliberate. It means the rules about what
counts as a valid request live in one place (the Parser), the rules about keeping
records live in one place (the Store), and the CLI is only wiring. When a
requirement changes, there is usually one obvious folder to open.

## ledger-command binding

The Command Parser is reached by running `parse/parse.sh` with the typed words as
its arguments, and reading its stdout. That stdout is one line of `key=value`
pairs, which is how the transport-neutral request-reply of `ledger-command` is
carried here: `action=add amount=-450 memo=coffee` on success, and
`refusal=UNKNOWN_ACTION detail=...` on a refusal. Exit code `0` means a decision
was reached, `2` means it was a refusal — matching TR-4.

## entry-recorded binding

The CLI does not receive `entry-recorded` notices over any channel. It reads them
off the Entry Store's stdout, one notice per line, in the same
`seq=… amount=… memo=… balance=…` form. Because the CLI runs the Store as a child
process and the Store announces before it exits, "the notice arrives after the
movement is durably written" is guaranteed by ordering within the one process,
with nothing to configure.
