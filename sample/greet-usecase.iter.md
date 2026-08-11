---
name: "User greets a friend, loudly"
description: "The full path: parse --shout, greet by name, verified by the contract group"
participants:
  - 1 src
  - 2 test
---

# Use-case: greet a friend, loudly

`./src/greet.sh --shout Ada` → `HELLO, ADA!` — implemented in Greet Core (step 1),
proven by the shout/contract test groups (step 2).
