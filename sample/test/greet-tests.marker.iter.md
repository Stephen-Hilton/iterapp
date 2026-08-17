---
name: Greet Tests
level: component
description: "Deterministic test groups driving greet through its CLI contract"
uses: [greet-msg]
---

# Greet Tests

Test groups live in `testgroup.iter.md` beside this marker; scripts are standalone
`testscriptNN.sh` executables printing `passed N/M` / `failed N/M`. Tests interact
with greet ONLY through the `greet-msg` interface — never by sourcing the script.
