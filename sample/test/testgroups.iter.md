# greet — test groups

Tests for the `greet` sample feature (see `../bizreq.md`, `../techreq.md`). Every script
here is deterministic: run from anywhere, it `cd`s to the component root itself, prints
`passed N/M` or `failed N/M`, and exits 0 only on a full pass.

## Groups

- **default greeting** — no-name behavior (BR-2) and single-line output discipline (TR-3).
  Generation prompt: add cases around empty-string args, whitespace-only args.
- **named greeting** — greeting by name (BR-1), friendliness contract (BR-3).
  Generation prompt: add cases for names with special shell characters, very long names.
- **contract** — exit codes and stream discipline (TR-3, TR-4).
  Generation prompt: when `--shout`/`--lang` land (BR-4/BR-5), add invalid-flag cases
  asserting non-zero exit and a one-line stderr message.

Run everything: `for t in test/testscript0*.sh; do sh "$t"; done` from `sample/`.

<!-- iterapp:testgroups
{"label":"default greeting","lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"2/2","testlist":["testscript01.sh"]}
{"label":"named greeting","lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"3/3","testlist":["testscript02.sh"]}
{"label":"contract","lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"2/2","testlist":["testscript03.sh"]}
-->
