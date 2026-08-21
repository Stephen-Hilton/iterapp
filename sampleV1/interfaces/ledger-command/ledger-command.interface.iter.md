---
interface: ledger-command
kind: request-reply
description: "What a person typed, turned into a decided command with its arguments already checked."
testgroup: test/testgroup.iter.md
test_dir: test
---

# ledger-command — contract

A line of words a person typed goes in. A decided command goes out: which action
was meant, with its arguments already checked. Anything undecidable comes back as
a named refusal instead, so no caller has to guess whether a half-understood
request is safe to act on.

## Request

```
{
  "argv": ["add", "-450", "coffee"]   // array of strings, required — the words as typed, in order
}
```

## Reply, success shape

```
{
  "action": "add",                    // string, one of: add | total | report
  "amount": -450,                     // integer cents, present only when action is "add"
  "memo": "coffee"                    // string, present only when action is "add"; never empty, never contains a newline
}
```

## Reply, failure shape

```
{
  "refusal": {
    "code":   "UNKNOWN_ACTION",       // one of: UNKNOWN_ACTION | MISSING_ARGUMENT | AMOUNT_NOT_INTEGER | EMPTY_MEMO
    "detail": "no action named 'delete'"
  }
}
```

## Worked examples

Normative — each pair must hold on every implementation (strict JSON):

```json
[
  { "request": { "argv": ["add", "-450", "coffee"] },
    "reply":   { "action": "add", "amount": -450, "memo": "coffee" } },

  { "request": { "argv": ["add", "125000", "paycheck for March"] },
    "reply":   { "action": "add", "amount": 125000, "memo": "paycheck for March" } },

  { "request": { "argv": ["total"] },
    "reply":   { "action": "total" } },

  { "request": { "argv": ["report"] },
    "reply":   { "action": "report" } },

  { "request": { "argv": ["delete", "3"] },
    "reply":   { "refusal": { "code": "UNKNOWN_ACTION", "detail": "no action named 'delete'" } } },

  { "request": { "argv": ["add", "450"] },
    "reply":   { "refusal": { "code": "MISSING_ARGUMENT", "detail": "add needs an amount and a memo" } } },

  { "request": { "argv": ["add", "4.50", "coffee"] },
    "reply":   { "refusal": { "code": "AMOUNT_NOT_INTEGER", "detail": "amount must be whole cents" } } },

  { "request": { "argv": [] },
    "reply":   { "refusal": { "code": "MISSING_ARGUMENT", "detail": "no action given" } } }
]
```

## Invariants

- Total: every possible `argv` produces either a success reply or a refusal,
  never nothing and never both.
- Deterministic: the same `argv` always produces the same reply. Nothing about
  the reply depends on the stored log, the clock, or the machine.
- The refusal codes are a closed vocabulary. A reply carrying a code outside
  `UNKNOWN_ACTION | MISSING_ARGUMENT | AMOUNT_NOT_INTEGER | EMPTY_MEMO` violates
  this contract.
- Deciding a command never records anything. A refusal and a success are equally
  free of side effects.
- `amount` and `memo` appear together or not at all, and only for `action: add`.
- Transport-neutral: these messages ride any carrier unchanged; carrier bindings
  (argument order, exit codes, stdout framing) live on the serving object's
  marker, never here.
