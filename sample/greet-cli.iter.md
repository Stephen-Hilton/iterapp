---
interface: greet-cli
kind: cli
endpoint: "./src/greet.sh [--shout] [name]"
description: "The greeter's command-line contract: args in, one line out"
---

# greet-cli — interface contract

- Invocation: `./src/greet.sh [--shout] [name]`
- stdout: exactly one line, `Hello, {name|World}!` (uppercased with `--shout`), no
  trailing whitespace (TR-3).
- Exit codes: 0 on success; 2 with a one-line stderr message for any unknown flag (TR-4).
- Stability: this contract is consumed by the test suite and any future callers;
  changes require updating every consumer in the same work item chain.
