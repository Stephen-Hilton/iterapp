---
name: Greet Core
level: component
description: "The greet.sh script: parsing, greeting table, output contract"
provides: [greet-msg]
---

# Greet Core

One POSIX script, `greet.sh` (TR-1/TR-2). Owns flag parsing, the greeting table, and
the stdout/exit-code contract (TR-3/TR-4). Agents working here must keep the
`greet-msg` interface contract intact — it is what the test suite drives.
