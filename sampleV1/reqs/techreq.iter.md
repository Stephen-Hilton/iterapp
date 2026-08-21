# Technical requirements — project-wide

Global technical requirements and constraints for **Sample Ledger**: stack
choices, conventions, and non-functional requirements that apply across the WHOLE
project. Component-local requirements stay beside their component
(`<component>/*.techreq.iter.md`); anything that spans components belongs here.

This file's location is the `global_techreq_path` setting in
`.iter/.engine/config.json` (default `{codepath}/reqs/techreq.iter.md`). Agents
get the resolved path as `$ITER_TECHREQ`. The engine surfaces this file and the
global bizreq to every agent automatically — exactly these two files, never a
directory scan.

This file needs no frontmatter — it is a plain context doc, never a map node.

- **TR-1** — Everything is POSIX shell run through `bash`. No compiler, no
  package manager, no network access: the whole sample must work on a bare macOS
  or Linux box with nothing installed.
- **TR-2** — Each C4 object is one directory that owns its own files: the marker
  file, the code, and a `test/` subtree holding the object's testgroup and test
  scripts. Nothing an object owns lives outside its directory.
- **TR-3** — Every executable component reads its input from arguments and writes
  its result to stdout. Diagnostics go to stderr. A component never prints
  diagnostics to stdout, because the calling component parses stdout.
- **TR-4** — Exit codes are the contract between components: `0` means the request
  was understood and carried out, `2` means the request was not understood.
  Nothing else is used.
- **TR-5** — The stored log is a single append-only text file. Its location is
  given to every component through the `LEDGER_FILE` environment variable, so
  tests can point at a scratch file instead of real data.
- **TR-6** — Amounts are whole cents, written as an integer. No decimal
  arithmetic anywhere, because shell has none.
- **TR-7** — Test scripts follow the iterapp test contract: exit `0` when
  everything held, `1` when something did not, anything else means the script
  itself broke. The last line of stdout is
  `ITER_RESULT pass=<n> fail=<n> total=<n>`.
- **TR-8** — Test scripts are deterministic and self-contained: each one creates
  its own scratch `LEDGER_FILE` under a temporary directory and removes it on the
  way out. No test depends on another test having run first.
