---
interface: greet-msg
kind: json
description: "The greeter's command-line contract: args in, one line out"
---


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
