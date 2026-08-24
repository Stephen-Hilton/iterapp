---
name: "Command Parser"
level: component
description: "Decides what a person meant by what they typed, and checks the arguments before anyone acts on them."
owner: bespoke
teststate: inherit
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  []
  inputs:     []
  outputs:    ["{topdir}/interfaces/ledger-command/ledger-command.interface.iter.md"]
  bizreqs:    ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Long Description

The Command Parser is the only part of the project allowed to have an opinion
about what a typed request means. It reads the words as they were typed and comes
back with one of two things: a decided command, or a refusal saying why it could
not decide.

Concentrating that judgment here is what makes BR-6 — "a confusing request must
never leave a half-written record behind" — cheap to guarantee. Deciding happens
before anything is written, in a part that has no ability to write, so a
misunderstood request physically cannot leave a mark.

There are three commands it can decide on:

- **add** — record a movement of money. It needs an amount and a note. The amount
  must be a whole number of cents, possibly negative; anything with a decimal
  point is refused, because the rest of the project does no decimal arithmetic
  (TR-6). The note is everything the person typed after the amount, joined back
  together, and it may not be empty.
- **total** — report the running total. Takes no arguments.
- **report** — print everything recorded, then the total. Takes no arguments.

Anything else is refused with a named reason. The full list of reasons and the
exact shape of both answers is the `ledger-command` contract in
`interfaces/ledger-command/`.

## ledger-command binding

This object serves `ledger-command` over process arguments and stdout. The
request's `argv` is the script's own arguments. The reply is one line of
`key=value` pairs on stdout: `action=add amount=-450 memo=coffee` for a decided
command, or `refusal=UNKNOWN_ACTION detail=<sentence>` for a refusal. Exit code
`0` carries a decision and `2` carries a refusal, per TR-4. The memo is written
last on the line so a note containing spaces needs no quoting.
