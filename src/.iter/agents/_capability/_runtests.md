# Capability: run tests and make claims (`iter runtests`)

`iter runtests` is the deterministic test runner: it runs a testgroup's shell
scripts, logs to `<test_dir>/runs/`, and updates the group's `lastrun`, `result`
and `counts` in its `testgroup.iter.md`. It is the only acceptance criterion for
"done" — never your own judgment that the code looks right.

    "$ITER_BIN" runtests --project "$ITER_PROJECT" --group "<label>" [--test <id>]

`--group` takes the label from the `iterapp:testgroups` JSONL block. `--test`
narrows a NEUTRAL run to one test id or script.

## Three modes: one neutral, two claims

- **Plain (neutral)** — no `--broken`, no `--fixed`. Runs and reports; it never
  flags anything. Run these freely while iterating.
- **`--broken`** — claims "the defect is still present". If the group is actually
  green the claim is false: the engine writes the fail-flag and the item fails at
  the turn boundary no matter what you do next. That means the item is STALE —
  touch no code and stop.
- **`--fixed`** — claims "the defect is resolved", and is the completion gate. Any
  red test or script error means the claim is false: fail-flag written, the item
  cannot close as done. The WHOLE group must be green — a fix that breaks a
  neighboring test is not done.

Claims always run the whole group; `--test` applies to neutral runs only.

## Defect items carry their failing testgroup (red before fix)

A work item may sit queued for hours and then run against a tree that has moved
on. In the TDD flow that risk is handled by tests, not prose: a defect-shaped
item carries the testgroup that proves the defect (`source_testgroup` on
sweep-born items; the group named in `mainwork` on items an agent authored). The
receiving agent reproduces BEFORE fixing — `--broken` first, then diagnose from
the failing tests' logs under `<test_dir>/runs/`, then fix the CODE, then
`--fixed` to gate completion.

A defect claim that could have a test gets the test written first, then the fix
item. Only for genuinely untestable claims (external infrastructure state,
credentials) may an item fall back to prose: state the claim, the check command,
and "if this no longer holds, report stale and stop" in `mainwork`.

## Script errors versus red tests

A test script exiting `1` means the test ran and something was unexpected — a red
test, and against unimplemented code that is expected and fine. An exit greater
than `1` means the script itself broke, which is a defect in the test, not in the
code under test. Fix script errors where you own the tests; report them where you
do not.
