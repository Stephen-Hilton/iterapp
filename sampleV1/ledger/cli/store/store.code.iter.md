---
name: "Entry Store"
level: component
description: "Writes each movement of money into the log for good, and announces that it did."
owner: bespoke
teststate: inherit
children:
  codedirs:   ["{thisfiledir}/"]
  codenodes:  []
  inputs:     ["{topdir}/interfaces/ledger-command/ledger-command.interface.iter.md"]
  outputs:    ["{topdir}/interfaces/entry-recorded/entry-recorded.interface.iter.md"]
  bizreqs:    ["{thisfiledir}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Long Description

The Entry Store owns the log. It is the only part of the project that writes to
it, which is what makes BR-2 — nothing recorded is ever silently changed or
dropped — something that can be checked by reading one folder.

The log is a plain text file whose location arrives in the `LEDGER_FILE`
environment variable (TR-5). Each movement is one line: the position it took, the
amount in cents, and the note. Writing is always an append; there is no code path
anywhere in the Store that rewrites or truncates the file, so the history is
append-only by construction rather than by discipline.

The Store also answers two questions about the log for whoever asks: what
position the next movement will take, and what everything so far adds up to. It
answers both by reading the file from the top, because a log this size does not
justify a cached total that could drift out of step with the file.

After a movement is safely on disk, the Store announces it as an `entry-recorded`
notice. The announcement comes strictly after the write, never before, so a
notice always describes something that really happened.

## entry-recorded binding

Notices are written to stdout, one per line, as `key=value` pairs in contract
order: `seq=2 amount=-450 balance=124550 memo=coffee`. The note is written last
so it needs no quoting. There is no channel and no acknowledgment — the caller
reads the line or ignores it. Because the write to `LEDGER_FILE` completes before
the line is printed, the contract's "only after the fact" invariant holds without
any coordination.

## ledger-command binding

The Store consumes the `add` form of `ledger-command` as its own arguments: the
amount as the first argument and the note as the second. It never sees the raw
words a person typed, and it does no checking of its own — by the time a request
reaches the Store, the Command Parser has already decided it is valid.
