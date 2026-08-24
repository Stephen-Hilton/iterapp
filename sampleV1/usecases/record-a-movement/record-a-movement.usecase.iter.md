---
name: "Record a movement"
description: "Someone spends or receives money and writes it down, then sees where that leaves them."
children:
  codenodes:  ["{topdir}/ledger/cli/cli.code.iter.md", "{topdir}/ledger/cli/parse/parse.code.iter.md", "{topdir}/ledger/cli/store/store.code.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# Record a movement

Someone has just bought a coffee for four dollars fifty. They open a terminal and
type the amount and a short note saying what it was for. A moment later the
program tells them the coffee is written down and what their money adds up to
now. That is the whole journey, and it is the one people take dozens of times for
every once they ask for a report.

Here is what happens between the typing and the answer.

The **Ledger CLI** (`ledger/cli`) receives what was typed. It does not try to
understand it. It hands the words straight to the **Command Parser**
(`ledger/cli/parse`), which is the only part allowed to decide what a request
means.

The Parser looks at the words and decides. It checks that there really is an
amount, that the amount is a whole number of cents rather than something with a
decimal point in it, and that a note was written. If any of that is wrong, it
refuses by name — and because the Parser cannot write anything, a request that
gets refused here has no way to leave a half-written record behind. The person
sees one plain sentence saying what was wrong, and their log is exactly as it was.

If the Parser is satisfied, it hands back a decided command: this is an `add`,
the amount is this, the note is this. The shape of that hand-off is written down
as the `ledger-command` contract.

The CLI passes the decided command to the **Entry Store**
(`ledger/cli/store`), the only part of the project that writes. The Store works
out what position this movement takes in the log — one more than the last one —
and appends a line. It only ever appends; there is no path through it that
rewrites or shortens the log, which is how "nothing recorded is ever silently
changed" stays true without anyone having to remember it.

With the line safely on disk, the Store announces what it did: the position, the
amount, the note, and what everything adds up to now. That announcement is the
`entry-recorded` contract, and it comes strictly after the write, so it never
describes something that did not happen.

The CLI reads the total out of that announcement and prints the confirmation.
Naming the new total right there is deliberate — it saves the person a second
command just to check the first one landed.
