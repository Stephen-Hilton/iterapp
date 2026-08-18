# itersched — scheduled work items

Built 2026-08-17. Three pieces: the `scheduled` work-item state, the `scheduler`
source, and the itersched engine (`src/itersched.rs`).

## Model

A schedule is an ordinary work item parked in `state: "scheduled"` — a TEMPLATE.
The loop never picks it (`eligible()` excludes it). Every 59s (minute
granularity by design) itersched asks each template whether it is due and, if
so, CLONES it into a normal queued run: fresh workid, `state: queued`,
`source: "scheduler"`, `source_schedule: <template id>`, priority inherited
from the template (the per-schedule urgency knob; the UI defaults it to 8 —
higher-is-sooner, so scheduled runs start soon after their moment).

`sched` spec on the template: `kind: every | daily | weekly | stale` with
`every_min`, `at: "HH:MM"`, `day: mon..sun`, `tz` (empty = user_timezone).
`stale` means "when no clone has COMPLETED within every_min minutes".

Rules, engine-owned (not UI conventions):

- **Dedup**: while ANY clone of a schedule is open (queued, in-progress,
  failed-awaiting-retry), the schedule does not fire. A terminally-failed clone
  is closed, so the next due moment fires a fresh clone.
- **Skip, never backfill**: daily/weekly occurrences missed while the engine
  was down are skipped (150s occurrence window vs the 59s check). Interval
  kinds fire at most once when overdue — that IS their normal behavior.
- **Queueing a schedule = clone-and-queue.** The API's `queue` action on a
  scheduled item fires it (409 if a run is still open); the template itself
  never enters the queue. `pause` stops the schedule, `schedule` resumes a
  todo/paused item that carries a spec, `complete` retires it.
- **Users only**: the webapp API accepts schedules; `iter add` (the agents'
  path) refuses `state: scheduled` or a `sched` spec.
- **Restart memory**: `sched.last_fired` on the template (workitems.jsonl) +
  the append-only audit log `.iter/.engine/sched_log.jsonl`.

## exec: shell — the second executor

`exec: "agent"` (default) runs a claude session; `exec: "shell"` makes the
engine run prework lines, mainwork, and postwork lines as `sh -c` commands in
the codepath — no LLM, no agent slot (own cap `engine.max_shell_workers`, per-
command budget `engine.shell_timeout_sec`), same lifecycle otherwise (codepath
lock, attempts/backoff, output capture, close-out). Shell items get the same
env contract as agents (ITER_BIN/ITER_PROJECT/ITER_REQS/ITER_TEST_DIR/
ITER_INTERFACE_DIR/ITER_WORKID), so `"$ITER_BIN" runtests --group X` just
works.

Additionally, `.iter/prepostwork/*.sh` files are engine-run shell steps INSIDE
agent items: resolved at their position, output captured and prefixed to the
next LLM turn's prompt ("run the tests, hand the agent the results"). The UI
flags them with a dashed ⚙ pill.

## Testing-framework merge — DONE 2026-08-17

The sweep's internal timer is gone (`scheduler.rs` sweep block and
`config.rs::TestingConfig` deleted). The sweep now runs as a "Test Loop"
scheduled workitem — `exec: shell`, `kind: every` / `every_min: 120`, mainwork
`"$ITER_BIN" testsweep --project "$ITER_PROJECT" --concurrency 3 --priority-red
4 --priority-green 2 --green-stale-hours 24 --group-timeout-min 20` — created
on demand by the webapp's Settings → Test → "Create TestLoop Schedule" button
(dedup by title; nothing is auto-seeded, so existing projects opt in with one
click). Every former `testing.*` setting is now a visible flag on that command
line; pausing the schedule replaces `test_sweep_active`. The item is
deliberately lockless (`codepath: "."` + `codepath_ignore: ["**"]`): the sweep
skips busy C4 objects itself, and a real whole-root lock would serialize the
engine. Full rationale: features/TDD.md "Engine Test Sweep".
