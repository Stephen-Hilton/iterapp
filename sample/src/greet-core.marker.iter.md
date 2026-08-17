---
name: Greet Core
level: component
description: "The greet.sh script: parsing, greeting table, output contract"
provides: [greet-msg]
---

# Greet Core

One POSIX script, `greet.sh` (TR-1/TR-2). Owns flag parsing, the greeting table, and
the stdout/exit-code behavior (TR-3/TR-4). Agents working here must keep the
`greet-msg` interface contract intact — it is what the test suite drives.

## greet-msg binding (CLI carrier — this object's mapping, not the contract)

How this script carries the transport-neutral `greet-msg` messages:

    request.name   ← first positional argument      (absent argument = absent field)
    request.shout  ← the --shout flag               (present = true)
    reply.greeting → stdout, exit 0                 (the one line, no trailing whitespace)
    reply.refusal  → one line on stderr, exit 2     (the line names the refused flag)
