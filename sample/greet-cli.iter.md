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

## Machine-readable contract

```json
{
  "invocation": "./src/greet.sh [--shout] [name]",
  "stdin": "ignored",
  "stdout": {
    "lines": 1,
    "pattern": "^(Hello|HELLO), .+(!)$",
    "trailing_whitespace": false
  },
  "exit_codes": {
    "0": "success",
    "2": "unknown flag; exactly one line on stderr"
  },
  "examples": [
    { "argv": [],                    "stdout": "Hello, World!",  "exit": 0 },
    { "argv": ["Ada"],               "stdout": "Hello, Ada!",    "exit": 0 },
    { "argv": ["--shout", "Ada"],    "stdout": "HELLO, ADA!",    "exit": 0 },
    { "argv": ["-x"],                "stderr_lines": 1,          "exit": 2 }
  ]
}
```
