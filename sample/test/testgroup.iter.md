# greet — test groups

Tests for the `greet` sample feature (see `../bizreq.md`, `../techreq.md`). Every test
is a shell script honoring the engine's test contract: run from anywhere it `cd`s to the
component root itself; **exit 0** = ran, everything as expected (expected-error tests
exit 0 when the app correctly rejects), **exit 1** = ran, something unexpected,
**anything else** = the script itself broke; the last stdout line is
`ITER_RESULT pass=N fail=M total=T`.

## Groups

- **default greeting** — no-name behavior (BR-2) and single-line output discipline (TR-3).
  Generation prompt: add cases around empty-string args, whitespace-only args.
- **named greeting** — greeting by name (BR-1), friendliness contract (BR-3).
  Generation prompt: add cases for names with special shell characters, very long names.
- **contract** — exit codes and stream discipline (TR-3, TR-4).
  Generation prompt: when `--shout`/`--lang` land (BR-4/BR-5), add invalid-flag cases
  asserting non-zero exit and a one-line stderr message.

Run a group deterministically: `iter runtests --project . --group "default greeting"`
(the engine's test sweep does the same on a schedule and records history in `runs/`).

<!-- iterapp:testgroups
{"label":"default greeting","desc":"no-name behavior (BR-2) and single-line output discipline (TR-3)","auto_fix":false,"lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"2/2","testlist":[{"id":"testscript01","name":"default greeting cases","desc":"no-arg greeting text and single-line output","shell":"testscript01.sh"}]}
{"label":"named greeting","desc":"greeting by name (BR-1), friendliness contract (BR-3)","auto_fix":false,"lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"3/3","testlist":[{"id":"testscript02","name":"named greeting cases","desc":"greeting with a name argument","shell":"testscript02.sh"}]}
{"label":"contract","desc":"exit codes and stream discipline (TR-3, TR-4)","auto_fix":false,"lastrun":"2026-08-11T00:00:00Z","result":"passed","counts":"2/2","testlist":[{"id":"testscript03","name":"cli contract cases","desc":"exit codes and stdout/stderr discipline","shell":"testscript03.sh"}]}
-->
