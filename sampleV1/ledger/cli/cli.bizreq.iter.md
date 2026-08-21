# Ledger CLI — business requirements

- **CLI-BIZ-001** — A person invokes everything the same way: the program name,
  then the action, then that action's arguments. There is no other way in.
- **CLI-BIZ-002** — When a request is refused, the person sees one plain sentence
  saying what was wrong, and nothing is recorded.
- **CLI-BIZ-003** — When a movement is recorded, the person sees a confirmation
  naming the amount, the note, and the new running total, so they never have to
  ask for the total separately just to check the record landed.
- **CLI-BIZ-004** — The program does exactly one thing per invocation. It never
  prompts, never waits for more input, and never keeps running after it has
  answered.
