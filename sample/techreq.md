# Technical requirements: greet

- TR-1: Implemented as a single POSIX shell script, `src/greet.sh`, executable directly
  (`./src/greet.sh [name]`).
- TR-2: No dependencies beyond POSIX sh utilities.
- TR-3: Output goes to stdout, exactly one line, no trailing whitespace.
- TR-4: Exit code 0 on success; non-zero with a one-line stderr message on invalid flags.
- TR-5: All behavior covered by deterministic test scripts under `test/`, grouped in
  `test/testgroups.iter.md` per iterapp conventions.
