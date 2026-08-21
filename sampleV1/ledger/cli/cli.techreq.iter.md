# Ledger CLI — technical requirements

- **CLI-TECH-001** — `ledger.sh` holds no rules of its own. Deciding what a
  request means belongs to the Command Parser, and keeping records belongs to the
  Entry Store; the CLI only routes between them.
- **CLI-TECH-002** — The CLI locates its parts by path relative to its own file,
  never relative to the directory the person happened to be standing in when they
  ran it.
- **CLI-TECH-003** — The CLI passes its exit code through unchanged from whichever
  part it delegated to, so TR-4 holds end to end.
- **CLI-TECH-004** — Every part is invoked as a child process reading arguments
  and writing stdout. The CLI never sources another script into itself, so no
  part can reach into the CLI's own variables.
