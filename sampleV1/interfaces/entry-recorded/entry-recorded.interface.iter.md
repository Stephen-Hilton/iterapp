---
name: "entry-recorded"
kind: event
description: "The notice that one money movement has been written down for good, and what it was."
owner: bespoke
children:
  bizreqs:    ["{thisfiledir}/{thisfilestem}/*.bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/{thisfilestem}/*.techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/testgroup.iter.md"]
---

# entry-recorded — contract

Announced once, after a movement of money has been written down and can no longer
be taken back. It carries the position the movement took, the amount, the note,
and what the log adds up to now — enough that a reader can keep an exact view
from these notices alone.

## Event

```
{
  "seq":     7,                       // integer ≥ 1, required — position in the log; 1 for the first movement ever
  "amount":  -450,                    // integer cents, required — negative is money out, positive is money in
  "memo":    "coffee",                // string, required — never empty, never contains a newline
  "balance": 120550                   // integer cents, required — what the whole log adds up to including this movement
}
```

## Worked examples

Normative — each event must be producible and acceptable on every implementation (strict JSON):

```json
[
  { "event": { "seq": 1, "amount": 125000, "memo": "paycheck for March", "balance": 125000 } },
  { "event": { "seq": 2, "amount": -450, "memo": "coffee", "balance": 124550 } },
  { "event": { "seq": 3, "amount": -124550, "memo": "rent", "balance": 0 } }
]
```

## Invariants

- One-way: there is no reply and no acknowledgment. A movement that could not be
  written down produces no notice at all — silence is the only signal, and the
  caller learns of the problem from the refusal on its own request.
- `seq` starts at 1 and rises by exactly 1 per notice, with no gaps and no
  repeats. A gap means a notice was lost; a repeat means one was sent twice.
- Ordering: notices arrive in `seq` order, which is the order the movements were
  recorded, which is the order the log itself holds.
- `balance` of a notice equals `balance` of the previous notice plus this
  notice's `amount`; the first notice's `balance` equals its own `amount`.
  Anyone receiving every notice in order can therefore keep an exact running
  total without reading the log.
- Exactly once, and only after the fact: a notice is announced only after the
  movement is durably written. There is no notice for a movement that was
  refused, and no movement in the log without a notice.
- Transport-neutral: these messages ride any carrier unchanged; carrier bindings
  (file appends, stdout lines, message topics) live on the serving object's
  marker, never here.
