---
interface: greet-msg
kind: json
description: "A greeting exchange: an optional name and an optional shout flag in; exactly one greeting or one refusal out"
---

# greet-msg — contract

One request message in, exactly one reply message out (a greeting XOR a refusal).
The field names, types, defaults, and rules below are the whole contract; any
carrier that preserves them honors this interface.

Request:

```
{
  "name":  "Ada",    // string, optional — absent means "World"
  "shout": true      // boolean, optional — absent means false
}
```

Reply, success shape:

```
{
  "greeting": "HELLO, ADA!"   // string, exactly one line, no leading/trailing whitespace
                              // shout=false → "Hello, <name>!"  (exact punctuation)
                              // shout=true  → that same string, fully uppercased
}
```

Reply, failure shape:

```
{
  "refusal": {
    "code":   "UNKNOWN_FIELD",  // closed vocabulary; today the only code
    "detail": "volume"          // exactly one line naming what was refused
  }
}
```

Worked examples (normative — each pair must hold on every implementation):

```json
[
  { "request": {},                               "reply": { "greeting": "Hello, World!" } },
  { "request": { "name": "Ada" },                "reply": { "greeting": "Hello, Ada!" } },
  { "request": { "name": "Ada", "shout": true }, "reply": { "greeting": "HELLO, ADA!" } },
  { "request": { "volume": 11 },                 "reply": { "refusal": { "code": "UNKNOWN_FIELD", "detail": "volume" } } }
]
```

- Total and exclusive: every possible request gets exactly one reply, and a reply
  carries `greeting` or `refusal`, never both, never neither. A request field
  outside the contract draws a refusal naming that field — never silently
  ignored, never half-answered.
- Deterministic and stateless: the same request always yields the same reply, and
  no request changes the outcome of any other.
- Transport-neutral: nothing above names a carrier. These messages may ride an
  HTTP body, a gRPC message, a queue record, argv/stdout, or an in-process
  struct — chained in any order — without renaming a field or changing a rule.
  How a given C4 object binds them to its carrier (routes, topics, flags, exit
  codes) is recorded on that object's marker, not here.
