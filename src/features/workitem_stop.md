# Feature: Stop an in-progress work item

Status: BUILT 2026-08-20 — the "errantly started" escape hatch: halt a running
work item mid-stream from the webapp, with an informed confirmation and a git
undo hint.

## The problem

Once the engine picks an item it owns it: the webapp's Actions menu offered
only Clone, and the only brakes were whole-engine (stop.signal / drain). An
item queued by mistake — wrong scope, premature, plain wrong — ran to
completion (or failure) while the user watched, possibly committing work
nobody wanted. Killing the engine to stop one item is a sledgehammer that
requeues everyone else's in-flight work too.

## Design

- **Delivery is a file flag** (`.iter/.engine/stopitem-<workid>.signal`), the
  same decoupled pattern as critfail/reject/stop.signal: the API server writes
  it, the engine consumes it. Works across the process boundary and survives
  either side restarting.
- **The kill is mid-turn, not turn-boundary.** The runner's wait loop polls
  the flag every ~200ms while a claude session (or an engine-run shell step,
  or an exec:shell item's command) runs, and kills the process group the
  moment it appears — a stop acts in under a second, not after a 20-minute
  turn finishes.
- **The stopped item lands in `todo`** with partial output kept and
  `STOPPED by user mid-run` in lasterror — the same human-review bucket as
  `iter reject`: work judged errantly started must be re-evaluated by a human,
  never retried automatically (retrying would restart exactly what the user
  halted) and never buried in the archive.
- **Undo point recorded at every run start:** the engine stamps
  `git_start_commit` on the item — HEAD of the item's codepath repo the moment
  work began (empty when the codepath is not in a git repo, i.e. no commit
  prior to starting). `git reset --hard <that sha>` undoes everything the run
  did, including any commits the agent already made.
- **Informed consent in the webapp.** Stop lives in the in-progress Actions
  menu behind a confirmation dialog that states: "This stops work mid-stream,
  and may result in partially completed work." — and, when `git_start_commit`
  exists, shows the exact undo command (`cd <codepath>` + `git reset --hard
  <sha>`). No baseline, no undo hint: the dialog only promises what git can
  actually deliver.
- **A late stop changes nothing.** If the flag lands after the final turn
  finished, the item still closes complete and the flag is consumed (logged),
  so it can never ambush a later run. Stale flags from crashed attempts are
  removed at run start, like critfail/reject.

## Surface

- `POST /api/workitems/<id>/action {"action":"stop"}` — 409 unless the item is
  in-progress; 200 `{"state":"stopping", "git_start_commit": "<sha|empty>"}`.
  Webapp-only surface by design: stopping is a human judgment call, so there
  is deliberately no `iter` CLI verb for agents to reach.

## Tests (per the house law: every guard proves it can fail)

- Runner unit: a present stop flag kills a 30s sleep within the poll cadence —
  remove the flag check and the test times out red.
- E2E (`ITER_CLAUDE_BIN` harness): a SLOW_TRIGGER item is stopped mid-turn via
  the flag → engine reaches idle promptly, item is `todo` with the STOPPED
  note, never closed; `git_start_commit` equals the repo's pre-run HEAD.
