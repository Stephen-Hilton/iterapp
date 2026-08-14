# Feature: Test-Driven Development loop

Status: DESIGN — agreed 2026-08-14, not yet built.
Owner intent: shift iterapp's center of gravity from "get everything integrated"
to a standing loop of (a) define tests, (b) run tests, (c) fix what fails, (d) repeat.

This note is the single place tracking the design, what exists today that
supports it, and — critically — the **vestigial code that must be removed or
rewritten when this lands**, so the transitional scaffolding doesn't calcify.

## Target flow

1. **User opens a plan work item** describing a new feature or use-case; submits.
2. **Plan agent** builds the plan (requesting a critical review via
   `iter critreview` before acting on it), producing:
   - plan md, the component marker `*.iter.md`, `bizreq.iter.md`,
     `techreq.iter.md`, `interfaces.iter.md`, and `testgroups.iter.md`
     (test group definitions only — **no tests**). The testgroups are the most
     review-critical artifact: they shape dev acceptance.
   - **Two work items in `todo` state (not queued):**
     - `code` — implement the approved plan
     - `testwriter` — create dozens of working tests per testgroup
3. **User reviews all documents**, corrects directly or opens follow-up items.
4. **User flips the todo items to queued** when satisfied — either order, or both
   at once (or asks one agent to queue the other).
   - code and testwriter run **in parallel** and must be **independently
     derived** from the docs (tests are not written to match the code, nor code
     to match the tests — both match the requirements).
   - ⚠ Parallelism requires **disjoint codepaths**: the codepath lock serializes
     overlapping scopes. Plan must scope code to `src/<component>` and
     testwriter to `tests/<component>` (or equivalent). Make this explicit in
     `plan.md` when building this out.
5. **code agent** implements from plan + marker + bizreq + techreq + interfaces
   + testgroups.
6. **testwriter agent** loops through every testgroup and writes dozens of tests
   per group: golden-path use-case tests, expected-error tests, edge-case tests.
   When done, marks the tests available for ongoing testing (registers them in
   the testgroup's `testlist`).
7. **Periodic sweep (deterministic engine job — not an agent):** wakes on a
   schedule and interrogates all testgroups across the project:
   - group with **no fully-green run ever** → create a `test` work item, priority 4
   - group with **no fully-green run in the last XX hours** (configurable) →
     create a `test` work item, priority 2
   - **Dedup guard (required):** do NOT create a test item for a group that
     already has an open `test` or `code` item — otherwise "failure empties the
     last-run timestamp" + the never-green rule spawns duplicates every wake-up.
8. **test agent** (transitional; see "deterministic test engine" below):
   - runs all tests in the group
   - all green → update `lastrun`/`result`/`counts` in the testgroup block, save
     results for reporting, done
   - any failure → create a `code` work item carrying the failure + preliminary
     diagnosis, empty the last-run timestamp
   - the created code item's premise IS the failing test: the receiving code
     agent re-runs it first; still red → fix; now green → stale, report + close.

## Engine pieces to build

- **`iter runtests --group <label> --expect pass|fail`** — engine runs the
  group's `testlist` via a per-project `test_command` config, updates the
  testgroup JSON block (`lastrun`, `result`, `counts`), and on the wrong outcome
  writes the same `critfail-<workid>.txt` fail-flag the scheduler already
  consumes (`scheduler.rs::critfail_path` / `take_critfail`). Same pattern as
  `iter critreview`: makes "done but tests fail" structurally impossible, like
  the fail-flag made "proceeded without review" impossible.
  - `--expect fail` is the red-step / premise mode: used at the start of a
    test-failure fix item to prove the defect still reproduces.
- **Periodic testgroup sweep** — tick-time scan in the engine (the machinery
  exists: `testgroups.rs` parses `label/lastrun/result/counts/testlist`; the
  `mod testgroups` comment in `main.rs` already anticipates "v2 scheduling").
  Config: sweep interval, staleness window XX hours, the two priorities, dedup
  guard above.
- **Deterministic test engine (maturity step):** replace the test *agent* with
  `iter runtests` invoked by the sweep directly; keep an agent only for the
  preliminary-diagnosis step after a deterministic run fails (or make diagnosis
  a follow-on item).
- **Config additions:** `test_command` template (e.g. `pytest {tests}`), sweep
  settings above.

## How premise-check fits (and dissolves)

Decision 2026-08-14: premise-check is **transitional scaffolding**, not a second
testing framework. In the TDD paradigm every defect-shaped item is born from a
failing test, and the premise collapses into red-green discipline:
**reproduce the failure before fixing; can't reproduce → item is stale → stop.**

Per item class in the target flow:
- user feature/plan items — describe future state; no premise.
- plan-created code/testwriter items — premise is the approved docs, pulled
  fresh by git-pull prework; no stamp.
- test-failure code items — premise is the failing test they carry; `iter
  runtests --expect fail` is the check.
- prose `holds-if` survives ONLY for genuinely untestable claims (infra state);
  a defect claim that could have a test gets the test written first instead.

## Vestigial code / prompts to REMOVE or REWRITE when TDD lands

The whole reason this file exists. Work the list top to bottom during rollout;
each unremoved item makes TDD dev weirder.

- [ ] `src/.iter/agents/_shared.md` — **rewrite** the "Premise stamp on work
      items that assert current state" section down to: defect items must carry
      their failing test; reproduce before fixing (`iter runtests --expect
      fail`); can't reproduce → stale → close. Drop the prose PREMISE/holds-if
      block format from the mainline convention (keep one line pointing at the
      untestable-claim escape hatch, or drop entirely).
- [ ] `src/.iter/prepostwork/premise-check.md` — **retire** (remove from
      template + `src/template.rs` TEMPLATE list) once no open items use
      `premise-check` prework. Its §3 fallback ("no PREMISE block: git log
      --since times.added") is the only part with lasting value — fold that
      into the test-failure item convention if still wanted.
- [ ] `sample/.iter/` mirrors of both files above.
- [ ] `src/.iter/agents/test.md` — **rewrite** for the sweep-driven role:
      today it's a generic "run the tests" agent; target is the step-8 contract
      (green → record + stop; red → spawn code item with failing test as
      premise + diagnosis). Later: shrink to diagnosis-only once `iter
      runtests` handles execution.
- [ ] `src/.iter/agents/testwriter.md` — **rewrite**: derive tests from
      testgroups/requirements docs independently of the code; golden-path /
      expected-error / edge-case mix; register tests in `testlist`; never read
      the implementation to decide what "correct" is.
- [ ] `src/.iter/agents/plan.md` — **update**: emit the document set from
      step 2, create the two `todo` items (not queued), give code/testwriter
      disjoint codepaths, request critical review of the plan + testgroups
      before creating the items.
- [ ] `src/.iter/agents/code.md` — **update**: TDD framing already half-exists
      ("run the test group first, expect failures"); align with `iter runtests
      --expect fail` premise mode for test-failure items.
- [ ] `#[allow(dead_code)] mod testgroups` in `main.rs` — the allow goes away
      when the sweep lands; if it doesn't, that's the smell this list exists
      to catch.
- [ ] pdy-dev deployment (`~/dev/pdy-dev/devops/.iter/`) — carries its own
      customized copies of `_shared.md` (premise section with incident data)
      and `premise-check.md`; migrate them the same way, but remember pdy-dev
      customizations are intentional and must be merged, not overwritten.

## Already in place (2026-08-14) that this builds on

- `todo` state exists and the engine leaves todo items alone (`workitems.rs::STATE_TODO`).
- `testgroups.rs` parses/updates the `<!-- iterapp:testgroups -->` JSONL block.
- Fail-flag machinery: `scheduler.rs::critfail_path`/`take_critfail` consumes
  flags at turn boundaries and fails items regardless of agent claims;
  `ITER_WORKID` is injected into every agent session.
- `iter critreview` — the synchronous-subprocess + fail-flag pattern
  `iter runtests` should copy (see `main.rs::cmd_critreview`).
- Codepath locks serialize overlapping scopes (the reason step 4 needs
  disjoint paths).
- Retry context: failed/requeued items keep partial output and retries see a
  "# Previous attempt" section (`scheduler.rs::previous_attempt_section`).
