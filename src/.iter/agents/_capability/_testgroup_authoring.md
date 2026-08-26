# Capability: author testgroups and test scripts

Read this before you create or edit any `*.testgroup.iter.md` file, or any test
script registered in one. Two different jobs use this file: DEFINING groups (what
must be proven — the plan and usecase agents) and WRITING the scripts that prove
them (the testwriter agent).

## The testgroup file

A `*.testgroup.iter.md` lives beside the object it tests, normally in that object's
test directory (`globalsettings.test_dir`, exported as `$ITER_TEST_DIR`, default
`test`). It holds two things:

1. **Markdown prose describing each group** — exactly what the group must prove:
   golden paths, expected errors, edge cases. This is the most review-critical
   artifact in the flow, because it is what "done" means for the code.
2. **An `iterapp:testgroups` JSONL block** — one line per group, with keys
   `label`, `desc`, `auto_fix` (default `false`), `lastrun`, `result`, `counts`,
   and `testlist`.

Get the current skeleton from the engine rather than writing one from memory:

    "$ITER_BIN" validate --file <path>.testgroup.iter.md --template

**Defining a group is not writing its tests.** A plan or usecase agent writes the
prose and the JSONL line with an EMPTY `testlist` — the sweep turns an empty
testlist into a testwriter authoring item, so coverage follows automatically.

## Registering a test in a group's `testlist`

Each script becomes a structured entry in its group's `testlist`:

    {"id": "test02", "name": "invalid accounts",
     "desc": "rejects a set of invalid accounts", "shell": "test02.sh"}

Registration is what makes a test exist to the engine's sweep. An unregistered
script never runs. **Never delete existing tests.**

Per group, write a MIX: golden-path use-case tests, expected-error tests, and
edge-case tests — dozens per group where the definitions call for it, within
`testwriter_min_tests_per_group` / `testwriter_max_tests_per_group` from
`.iter/.engine/config.json`.

## The test contract (every test is a shell script)

- One script per test, in the component's test directory. The script may invoke
  anything — pytest, cargo test, curl, a mix.
- **Exit code**: `0` = ran, everything as expected (an expected-error test exits 0
  when the app correctly rejects!). `1` = ran, something unexpected. Anything
  else = the script itself broke. Never encode "expected failure" in the exit
  code — that logic lives INSIDE the script.
- **Last stdout line**: `ITER_RESULT pass=X fail=Y total=Z`.
- stderr is free-form diagnostics — make failures loud and specific there.
- Deterministic: same inputs, same result, every run. No timing dependence, no
  live network, no ordering assumptions.

## Three test FLAVORS, by what declares the group

- **code node file (C4 object)**: unit/component tests of that object's behavior.
- **use-case file**: end-to-end JOURNEY tests — scripts that walk the actual user
  journey through the real participants, in order.
- **interface file**: CONTRACT-enforcement tests — scripts that assert the real
  providers' inputs/outputs against the contract's example in the interface file
  body, so drift turns red instead of silently accumulating.

## The registration chain — make sure it is complete

The DECLARING file — a `*.code.iter.md`, `*.usecase.iter.md`, or
`*.interface.iter.md` (the sweep walks all three) — must LINK its tests via
`children.testgroups` (paths or globs resolved relative to the declaring file, e.g.
`["{thisfiledir}/test/*.testgroup.iter.md"]`); a link matching nothing means the
sweep never runs them. A testgroups link matching nothing therefore says "this
object is deliberately untested" — if tests should exist, create them where the
link will find them.

- If the declaring file's testgroups link would not match your new file: name the
  testgroup file so the existing glob finds it, or add/extend the
  `children.testgroups` entry. For a testwriter this is the one sanctioned write
  outside your codepath — you may edit exactly that children sub-key on your work
  item's declaring file, and touch nothing else in it. (`testgroups: []` declared
  empty is the deliberate opt-out for use-cases/interfaces — set it only when the
  item asks you to.)
- If the declared `testgroup.iter.md` does not exist: CREATE it, per the shape
  above.

## Prove every new script LAUNCHES

Run the group once via a neutral `iter runtests` (see `_runtests.md` — neutral runs
never flag anything). Failures against unimplemented code are expected and fine
(exit 1); script errors (exit greater than 1) are yours to fix now.
