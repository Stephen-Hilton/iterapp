# Feature: Test-Driven Development loop

Status: DESIGN AGREED 2026-08-15 — building now.
Owner intent: shift iterapp's center of gravity from "get everything integrated"
to a standing loop of (a) define tests, (b) run tests, (c) fix what fails, (d) repeat.

This note is the single place tracking the design, what exists today that
supports it, and — critically — the **vestigial code that must be removed or
rewritten when this lands**, so the transitional scaffolding doesn't calcify.

Decisions locked 2026-08-15 (discussion with owner):
- **No transitional test agent.** The sweep drives the deterministic itertest
  engine directly; agents only diagnose/fix, never execute test runs ad hoc.
- **Every test is a shell script** (see Test Contract). No `test_command`
  templating, no per-language config: polyglot is the script's problem.
- **`--broken` / `--fixed` claim flags** replace the earlier `--expect
  pass|fail` idea. Plain invocation is neutral and never writes fail-flags.
- **Fix items are per-testgroup**, carrying `source_testgroup` (+
  `source_tests` snapshot, informational). Dedup key = the group label.
- **auto-fix gates the state, not the item**: failure always births a fix
  workitem; auto_fix=true → `queued`, auto_fix=false → `todo` (human review).
- **Escalate-to-plan** is the answer to fixes too big for one session — not
  longer timeouts, not subagents.

## Target flow

1. **User opens a plan work item** describing a new feature or use-case; submits.
2. **Plan agent** detects the "new feature or use-case" and builds the plan (requesting a critical review via
   `iter critreview` before acting on it), producing:
   - `buildplan.md`, full description of what is to be built
   - files: the component marker `*.iter.md`, `bizreq.iter.md`,
     `techreq.iter.md`, `interfaces.iter.md`, and `testgroup.iter.md`
     (test group definitions only, aka describing the tests needed — **no tests**). The testgroups are the most
     review-critical artifact: they shape dev acceptance.
   - **Two work items in `todo` state (not queued):**
     - `code` — implement the approved plan
     - `testwriter` — create dozens of viable, run-able tests per testgroup
3. **User reviews all documents**, corrects directly or opens follow-up items.
4. **User flips the `todo` items to `queued`** when satisfied — NOT order dependent.
   - code and testwriter run **in parallel** and must be **independently
     derived** from the docs (tests are not written to match the code, nor code
     to match the tests — both match the requirements).
   - Parallelism requires **disjoint lock scopes** — code agent gets the `codepath`
     with `"codepath_ignore": ["<test_dir>/"]` (test subtree carved out of its lock; it
     must not touch them) and the testwriter agent locks `<component>/<test_dir>`.
     The test directory name is definitive, not guessed: `globalsettings.test_dir`
     (default `test`), exported to agents as `$ITER_TEST_DIR`. `plan.md`,
     `testwriter.md`, and `_shared.md` carry the matching instructions.
   - **Deterministic guard (engine, not prompt):** the scheduler refuses to
     start a `code` item whose lock scope overlaps an open `testwriter` item's
     `<component>/<test_dir>` scope (and vice versa). Disjointness is enforced
     structurally, same spirit as the fail-flag.
5. **code agent** implements from plan + marker + bizreq + techreq + interfaces + testgroups.
6. **testwriter agent** loops through every testgroup and writes dozens of tests
   per group: golden-path use-case tests, expected-error tests, edge-case tests.
   When done, registers each test in the testgroup's `testlist`.

## Test Contract (locked)

Every test is a **shell script** living in `<component>/<test_dir>/`
(`globalsettings.test_dir`, default `test`). The script may invoke anything —
pytest, cargo test, curl against the webapp, a mix. The engine's only contract
with it:

- **Exit code**: `0` = ran, all assertions as expected (green). `1` = ran,
  some assertion unexpected (red). **Anything else** = the script itself broke
  (missing dep, crash, timeout kill) → recorded as `error`, a distinct state
  from red, so infrastructure problems don't spawn bogus fix items.
- **Last line of stdout, machine-readable**: `ITER_RESULT pass=12 fail=2 total=14`
  — gives the engine counts for the testgroup block and `tests XX/YY Z%` display.
- Everything else on stdout, plus **all of stderr (free-form diagnostics)**, is
  captured verbatim to `<test_dir>/runs/<timestamp>-<testid>.log` — the run
  history the UI walks and the diagnosis goldmine for fix agents. Timestamped
  filenames make time-based archive/delete trivial.
- **Inverted/expected-error logic lives inside the script**: a test that
  verifies "invalid login is rejected" exits 0 when the app correctly rejects.
  The engine never interprets test intent.

`testlist` entries are structured objects, not bare strings:

```json
{"id": "test02", "name": "invalid test accounts", "desc": "reject a set of invalid accounts", "shell": "test02.sh"}
```

Hierarchy example:

- testgroup = {"label":"Webapp Auth", "desc":"authenticate to the webapp: valid accounts, invalid accounts, edge cases (expired password, invalid characters, ...)", "auto_fix": false, ...}
  - {"id":"test01", "name":"valid test accounts",   "shell":"test01.sh"}
  - {"id":"test02", "name":"invalid test accounts", "shell":"test02.sh"}
  - {"id":"test03", "name":"edge cases",            "shell":"test03.sh"}

## `iter runtests` — three invocation modes, two armed

The runner is one command; a two-valued *claim* flag tells the engine which
exit code should end the workitem at this point in its lifecycle. The flag
names the claim the agent is making; a false claim writes the same
`critfail-<workid>.txt` fail-flag the scheduler already consumes
(`scheduler.rs::critfail_path` / `take_critfail`) — enforcement regardless of
what the agent says. Plain invocation makes no claim, so no punishment.

- **`iter runtests --group <label> [--test <id>]`** — neutral. Run, log to
  `runs/`, update the testgroup block, report. Never writes a critfail flag.
  Used by the sweep (no `ITER_WORKID` → red creates a workitem instead) and by
  agents mid-fix to check progress, freely and safely.
- **`iter runtests --group <label> --broken`** — the agent asserts "the defect
  is still present" (premise step, before editing). Group fully green → claim
  false → item is stale → critfail flag. `error` results also refuse the
  claim: "test couldn't run" must not pass for "defect reproduces".
- **`iter runtests --group <label> --fixed`** — the agent asserts "the defect
  is resolved" (completion gate). Any red in the group → claim false →
  critfail flag; the item cannot close as done. Whole group must be green —
  a fix that breaks a neighbor can't close the item.

Enforcement contract is **group-level** (per-group fix items make every red in
the group this item's business). `--test <id>` narrows a neutral run for
progress-checking; claims always evaluate the whole group.

Maturity step (later, not v1): the scheduler itself runs the `--fixed` gate
when an agent claims completion, closing even "agent forgot to run the gate".

## Engine Test Sweep (deterministic — no agent)

**Reworked 2026-08-17 (owner decision): the sweep is driven by itersched, not by
an engine-internal loop, and its knobs are CLI flags, not config.json settings.**
The old `testing` section of config.json (`test_sweep_active`,
`minutes_between_test_sweeps`, `test_green_stale_hours`,
`test_sweep_timeout_minutes`, `parallel_test_concurrency`,
`workitem_priority_lastrun_not_green`, `workitem_priority_lastrun_green`) is
GONE — `config.rs::TestingConfig` deleted, the sweep block removed from
`scheduler.rs`. What replaced each piece:

- **The loop**: a user-created "Test Loop" scheduled workitem (`state:
  scheduled`, `exec: shell`, `sched: every 120 min`) whose mainwork runs
  `"$ITER_BIN" testsweep --project "$ITER_PROJECT" --concurrency 3
  --priority-red 6 --priority-green 8 --green-stale-hours 24
  --group-timeout-min 20`. One click creates it: webapp Settings → Test →
  "Create TestLoop Schedule" (refuses a duplicate by title). Editing the
  workitem's command line IS the configuration; every knob is a visible flag.
- **`test_sweep_active`** → pause/resume the schedule (paused templates never fire).
- **`minutes_between_test_sweeps`** → the template's `sched.every_min`.
- **Overlap protection** → itersched's open-clone dedup (one open run per
  schedule) replaces the old `sweep_in_flight` flag.
- **Lock scope**: the Test Loop item carries `codepath: "."` with
  `codepath_ignore: ["**"]` — everything carved out, effectively lockless. The
  sweep needs no lock of its own (it skips busy C4 objects per-object,
  `testsweep.rs`), and a whole-code-root lock would serialize the engine.
- **`test_sweep_timeout_minutes`** → `--group-timeout-min` on `iter testsweep`
  and `--timeout-min` on `iter runtests` (`runtests::DEFAULT_GROUP_TIMEOUT_MIN`,
  default 20 — raised from 10, owner call 2026-08-17). The compiled default
  exists because the budget guards EVERY runtests invocation — agents' `--broken`
  / `--fixed` gates and manual runs included, not just the sweep.
- The remaining flags default in `testsweep.rs::SweepOptions` (3 / 6 / 8 / 24).

**Priorities are LOWER-IS-SOONER since 2026-08-17** (P0 = most urgent, default
5 — inverted to the industry convention; `iter invert-priorities` migrates an
existing open queue as newP = 10 - P). Sweep-born fix items default numerically
ABOVE 5 (red 6, stale-green 8), so they fill idle capacity instead of starving
user work.

Behavior deltas from the old loop: no more first-pass-~90s-after-start (the
schedule fires on its own anchor; skip-don't-backfill applies after downtime),
and a draining/holding engine doesn't fire schedules — same effect as the old
"draining engines don't sweep".

**File roles are FILENAME-derived (decided 2026-08-16, second decision that day).**
The word right before `.iter.md` declares what a file IS — `marker` / `bizreq` /
`techreq` / `interface` / `testgroup` / `usecase`, all singular, any prefix
(`gateway.marker.iter.md`). Frontmatter supplies attributes, never identity: a
stray `level:` inside a usecase file changes nothing; renaming is the only way to
change role (`markers::role_of`). Unrecognized suffix = plain context doc.

**Discovery is marker-driven (decided 2026-08-16).** The marker file defines the
C4 object; every file belonging to it is DECLARED in the marker's frontmatter,
never inferred from directory positions:

    testgroup: test/testgroup.iter.md   # MANDATORY for the sweep; path relative to the marker file
    test_dir: test                        # subtree with the test scripts (testwriter's lock scope / code items' carve)
    bizreq: bizreq.iter.md                # optional; webapp lightboxes use these
    techreq: techreq.iter.md

No `testgroup:` key on a MARKER = that C4 object is DELIBERATELY outside the
sweep (some work shouldn't be tested; absence is a choice, not an accident).
testgroup.iter.md files no declaring file claims are listed as "unowned" and
never run. The testwriter's registration-chain duty: add the
`testgroup:`/`test_dir:` keys to the declaring file if missing (its one
sanctioned write outside its codepath), and create the declared file if
missing — so "add tests to this thing" is a one-item ask.

**The sweep universe is markers + use-cases + interfaces (2026-08-17).**
`*usecase.iter.md` and `*interface.iter.md` files declare testgroups with the
same `testgroup:`/`test_dir:` keys — use-cases get end-to-end JOURNEY tests
(scripts that walk the actual user journey; user-centric TDD's steering
signal), interfaces get CONTRACT-enforcement tests (scripts asserting the real
providers' I/O against the contract's example, so drift turns red instead of
silently accumulating — interfaces are enforcement, not documentation). The
missing-key rule INVERTS for these two kinds: tests are their point, so no
`testgroup:` key is a coverage GAP that births a testwriter authoring item;
the explicit `testgroup: none` is the deliberate opt-out. Red runs of these
groups span C4 objects, so their fix items scope to the code root (auto_fix
false → todo, where a human can narrow the codepath) with diagnose-locally-or-
escalate-to-plan guidance. The usecase agent creates each use-case as a FOLDER
(`usecases/<name>/` holding the usecase file + a `<test_dir>/` subtree, no
marker — the same folder-owns-its-files law as C4 objects) and declares the
E2E testgroup (with empty testlists) at creation, so coverage follows
automatically.

**Non-convergence guard (2026-08-17):** escalated plans carry
`--source-testgroup "<label>"`, and `iter add` counts the laps — the fix →
plan → build → still-red cycle may run twice; the THIRD plan born from the
same testgroup is held in `todo` with a NON-CONVERGENCE note so a human
reconsiders the approach instead of the loop grinding.

Sweep, each wake (`testsweep.rs`; fired by the "Test Loop" scheduled workitem
described above; manual: `iter testsweep`):

1. Scan marker + use-case + interface files (projects.json scan_roots +
   marker_glob), follow each `testgroup:` key, and interrogate the declared
   groups:
   - **last run not green** (never ran, red, or error) → candidate, priority
     `--priority-red` (default 6)
   - **green but older than `--green-stale-hours`** → candidate, priority
     `--priority-green` (default 8)
   - **declared testgroup file missing, or a group with an empty testlist**
     (2026-08-17 flow) → ONE testwriter authoring item in `todo` (human gate;
     dedup via `source_testgroup` — the group label, or the declared path when
     no group exists yet). The deterministic sweep never judges minor-vs-major:
     if the testwriter finds no CODE to test either, IT escalates to a plan
     item carrying its gap analysis, mirroring the code agent's
     escalate-to-plan.
   - a C4 object with an active codepath lock is skipped (mid-edit trees would
     produce meaningless results); empty testlists are skipped (that's a
     testwriter gap, not a red run)
   - fix items are scoped to the MARKER FILE's directory with the declared
     `test_dir/` carved out — layout-independent (works for `<object>/test/`
     and for PDY-TECH-030's testgroup.iter.md-at-object-root alike)
2. Run candidates through the itertest engine (respecting concurrency +
   per-group timeout). **Always record the run** — even when item creation is
   suppressed below — so `runs/` history and the UI stay truthful.
3. Per result:
   - **green** → update `lastrun`/`result`/`counts`; if an *unstarted*
     (todo/queued) sweep-born fix item exists for this group, **auto-close it
     as stale** (deterministic stale-item cleanup; items an agent has started
     are left alone).
   - **red** → create ONE fix workitem per group (`code` type; agent may
     escalate to plan — see below) carrying `source_testgroup` + the
     `source_tests` snapshot of what was red; state `queued` if the group's
     `auto_fix` else `todo`.
   - **error (exit >1)** → create a `testwriter` workitem (fix the broken
     test) in `todo` for review.
4. **Dedup guard (required):** no new item when an open (todo / queued /
   in-progress / failed-pending-retry) item already carries the same
   `source_testgroup`. While the existing item is still *unstarted*, the sweep
   may refresh its `source_tests` snapshot in place (nicety, not contract).
   Manually opened items do NOT suppress sweep items — a refactor item makes
   no promise about test02.

## Provenance fields (workitems)

Sweep-born items carry:

```
source_testgroup: Webapp Auth
source_tests: [test02, test05]     # informational snapshot: red at birth
```

`source_testgroup` is the dedup key, the `--broken`/`--fixed` target, and the
UI's link from run history → workitem. `source_tests` is a diagnosis starting
point only — enforcement is group-level.

## Fix items too big for one session: escalate-to-plan

When the fix agent diagnoses the failure and the fix is comprehensive (spans
components, needs design decisions, won't fit a session), it does NOT grind
against timeouts and does NOT spawn subagents. It **creates a plan workitem**
carrying its diagnosis + the `source_testgroup` provenance, then closes its
own item as escalated. The plan agent decomposes into normal workitems — each
with its own timeout, retry, lock scope, spend tracking, and UI visibility.
The escalated plan item inherits the state its parent was born with
(auto_fix on → queued, off → todo).

Existing mitigations that soften big-but-mechanical fixes: retries keep
partial output (`scheduler.rs::previous_attempt_section`), and committed
progress means already-green tests shrink the next attempt's scope.

## UI to build

Enhancements to the iterapp project/C4 user interface, to accommodate TDD and
patch a few other requirements. Much of the markerfile data becomes 1st-class
data points in the UI (textboxes, buttons opening breakdown lightboxes). All
interfaces allow **edits** (whole-section replace on save) so they serve
maintenance, not just reading — the ONLY exceptions are the global
`bizreq.iter.md` and `techreq.iter.md` (the `global_bizreq_path` /
`global_techreq_path` settings, default `{codepath}/reqs/`), which are
read-only in the UI, **with their file location visible** so out-of-band
edits are easy to find.

Per C4 Object, collapsed/expanded detail section:

- existing fields present today: MarkerFile, CodePath, UseCases (uses/provides)
- **short description**: first item; full text readable (today's header form is
  too abbreviated).
- **long description**: longer plain-language description of the C4 object
  (context/container/component — exclude code):
  - describe, don't state; no jargon; acronyms defined on first use
    ("three letter acronym (TLA)"); simple enough for a non-technical reader
  - references to other parts of the project hyperlink to that C4 object
  - if absent in the markerfile: a **one-time sweep** stubs
    `# Long Description:\nTBD` into all found marker files, so a future plan
    item can target `contains "Long Description: TBD"`. Future C4 objects get
    agent-written long descriptions at creation (plan.md instruction), never
    TBD. The UI does not write stubs on render.
- **biz requirements** lightbox (wide, two sections, adjustable boundary):
  top = local `bizreq.iter.md` (editable); bottom = the global bizreq file
  (`global_bizreq_path` setting; read-only, path shown).
- **tech requirements** lightbox: same shape, `techreq.iter.md` pair.
- **uses/provides**: two horizontally tiled listboxes ("uses" / "provides")
  of references; clicking a name opens its detail (similar lightbox for a C4
  object, interface content for an interface). Navigation **replaces** the
  lightbox (infinite drill-down) with a **breadcrumb** trail to walk back.
  (Replaces today's giant unclickable string of technical names.)
- **testgroups** ("testing interface" lightbox): testgroups as left pane;
  tests (by name/description) and run detail/history in the main body.
  - clear description of what each testgroup and each test script tests
  - ongoing history from `<test_dir>/runs/` (timestamped files; archive/delete
    by time later)
  - **auto-fix flag** per group, defaults FALSE: gates whether sweep-born fix
    items arrive `queued` (true) or `todo` (false). Both paths end in a
    workitem; the flag only decides whether work proceeds without human review.

## How premise-check fits (and dissolves)

Decision 2026-08-14 (refined 2026-08-15): premise-check is **transitional
scaffolding**, not a second testing framework. In the TDD paradigm every
defect-shaped item is born from a failing test, and the premise collapses into
red-green discipline: **reproduce the failure before fixing; can't reproduce →
item is stale → stop.**

Per item class in the target flow:
- user feature/plan items — describe future state; no premise.
- plan-created code/testwriter items — premise is the approved docs, pulled
  fresh by git-pull prework; no stamp.
- sweep-born fix items — premise is the red testgroup they carry;
  `iter runtests --group <label> --broken` is the check, engine-enforced.
- prose `holds-if` survives ONLY for genuinely untestable claims (infra state);
  a defect claim that could have a test gets the test written first instead.

## Vestigial code / prompts to REMOVE or REWRITE when TDD lands

The whole reason this file exists. Worked top to bottom on 2026-08-15:

- [x] `src/.iter/agents/_shared.md` — premise section rewritten to the
      red-green discipline (`--broken` reproduce, `--fixed` gate, stale → stop);
      one-paragraph escape hatch kept for genuinely untestable claims.
- [x] `src/.iter/prepostwork/premise-check.md` — retired (file deleted,
      removed from `template.rs` TEMPLATE).
- [x] `sample/.iter/` mirrors — agent files re-mirrored; test.md and
      premise-check.md deleted; sample seed queue's `test` item removed;
      sample scripts/testgroups upgraded to the ITER_RESULT contract and
      structured testlist entries.
- [x] `src/.iter/agents/test.md` — retired (no transitional test agent; the
      deterministic engine runs tests). If a diagnosis-only agent proves
      useful later, write it fresh against the runs/ logs.
- [x] `src/.iter/agents/testwriter.md` — rewritten: independence rule,
      shell-script contract, structured testlist registration, repair items.
- [x] `src/.iter/agents/plan.md` — rewritten: TDD document set, two `todo`
      items, disjoint scopes, critreview before item creation, real long
      descriptions (never TBD).
- [x] `src/.iter/agents/code.md` — rewritten: `--broken` premise / `--fixed`
      gate / neutral mid-runs, never touch the test subtree, escalate-to-plan.
- [x] `#[allow(dead_code)] mod testgroups` in `main.rs` — allow removed; the
      module is live engine code (runtests.rs / testsweep.rs).
- [ ] pdy-dev deployment (`~/dev/pdy-dev/devops/.iter/`) — carries its own
      customized copies of `_shared.md` (premise section with incident data)
      and `premise-check.md`; migrate them the same way, but remember pdy-dev
      customizations are intentional and must be merged, not overwritten.
      (Deliberately NOT done here — separate repo, needs a merge, not a copy.)

## Already in place (2026-08-14) that this builds on
But confirm anyway.

- `todo` state exists and the engine leaves todo items alone (`workitems.rs::STATE_TODO`).
- `testgroups.rs` parses/updates the `<!-- iterapp:testgroups -->` JSONL block.
- Fail-flag machinery: `scheduler.rs::critfail_path`/`take_critfail` consumes
  flags at turn boundaries and fails items regardless of agent claims;
  `ITER_WORKID` is injected into every agent session.
- `iter critreview` — the synchronous-subprocess + fail-flag pattern
  `iter runtests` copies (see `main.rs::cmd_critreview`).
- Codepath locks serialize overlapping scopes (the reason step 4 needs
  disjoint paths).
- Retry context: failed/requeued items keep partial output and retries see a
  "# Previous attempt" section (`scheduler.rs::previous_attempt_section`).
