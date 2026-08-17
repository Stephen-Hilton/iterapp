---
name: Greet Tests
level: component
description: "Deterministic test groups driving greet through its CLI contract"
uses: [greet-cli]
---

# Greet Tests

Test groups live in `testgroups.iter.md` beside this marker; scripts are standalone
`testscriptNN.sh` executables printing `passed N/M` / `failed N/M`. Tests interact
with greet ONLY through the `greet-cli` interface — never by sourcing the script.
