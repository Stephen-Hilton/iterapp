# Feature: Work-item dependencies

Status: BUILT 2026-08-19 — implemented as specified (dispatch gate in pick_next before
lock checks; failed-dep flip to todo each tick; `created_by` stamped by `iter add` from
$ITER_WORKID; suffix resolution + cycle refusal shared by CLI and webapp API; blocked-by
in `iter status` and the webapp header chips; plan agent handoff docs updated).
Originally authored at Stephen's direction (from the pdy-dev session that sequenced
seven work items into three manual "waves").
Owner intent: declare "B runs after A finishes" ON the work items themselves, so a batch
of interdependent work can all be queued at once and sort itself out — no human (or
coordinating agent) babysitting wave boundaries overnight.

## The problem, from the night that motivated this

Seven work items existed in pdy-dev with real ordering constraints: one root-locked
"module homing" item had to finish before four service-scoped items could run (they build
code the homing item makes buildable), and a second root-locked relocation item belonged
last. Codepath locks made this FILE-safe but not ORDER-safe: locks know which items may
not run *simultaneously*, not which items must not run *yet*. Dispatch is by priority
among unblocked items, so queueing everything at once would have started an intake-scoped
fix (priority 7) against a tree that could not compile, while the item that fixes the
tree (priority 8, root-locked) waited politely behind an unrelated devops lock. The
workaround was a detached watcher script polling for queue-drain and then flipping states
— exactly the babysitting this feature deletes.

## Design

One new optional field on a work item:

    "depends_on": ["<workid-or-unique-suffix>", ...]

- **Dispatch gate.** A `queued` item with `depends_on` is not dispatchable until every
  dependency is SATISFIED. It stays visibly queued, with the blocker shown (see UI).
  Dependencies are evaluated BEFORE lock checks; locks and priority behave exactly as
  today among the items whose dependencies are satisfied.
- **Satisfied means the dependency closed COMPLETE — and its descendants too.** This is
  the subtle half, learned the hard way: a plan item "completes" the moment it spawns its
  children, long before the work it represents is done. So a dependency on item A is
  satisfied only when A is closed complete AND every item A created (matched by the
  engine recording a `created_by: <workid>` on children at add time — a new field the
  engine writes when an agent's `iter add` runs inside a work item) is itself closed,
  transitively. A flag `"depends_on_shallow": true` opts out for the rare caller who
  really wants plan-item-completion only.
- **A FAILED dependency never releases the dependent.** If A closes failed (attempts
  exhausted), everything depending on A flips to `todo` with a note naming the failed
  dependency — human review, never a silent run on a broken foundation, and never a
  silent hang either. (Mirrors the TDD decision that failure births reviewable work.)
- **`todo` semantics unchanged.** Dependencies gate dispatch of QUEUED items only;
  a todo item with dependencies is still just parked until a human queues it.

## CLI / JSON surface

- `iter add --file item.json` accepts the `depends_on` array; `iter add --depends-on
  <id>` (repeatable) for the flag path.
- IDs resolve by unique suffix — Stephen's convention is the LAST 12 characters (what the
  webapp header shows); any unambiguous suffix works, an ambiguous or unknown suffix
  REFUSES the add (exit 2) rather than guessing.
- `iter add` refuses a `depends_on` that creates a cycle (walk the graph at add time;
  refusal names the cycle path). A dependency may name a closed item (satisfied
  immediately) — useful for idempotent re-adds.
- `iter status` shows `blocked-by: <id12>` on gated items; the webapp work-item header
  shows the chain (blocked-by / blocks counts), since the header is what gets scanned.

## What this replaces (vestigial once landed)

- The detached watcher-script pattern (poll queue drain → stop engine → flip states →
  restart engine). Delete on sight once dependencies exist.
- Coordinating agents editing `workitems.jsonl` states directly with the engine stopped —
  the write-race dance exists only because ordering has no first-class home.

## Tests (per the house law: every guard proves it can fail)

- A queued item with an unsatisfied dependency is never dispatched: fake-agent engine run
  (`ITER_CLAUDE_BIN` probe harness) with A in-progress and B depends_on A — B must not
  dispatch; break the gate and watch the test go red.
- Transitive satisfaction: A (plan) completes but its created child C is open → B stays
  blocked; C closes → B dispatches. Shallow flag inverts this.
- Failed dependency: A closes failed → B lands in `todo` carrying the note, never
  dispatches. Cycle refusal: add A→B→A refused with the path named, exit 2. Suffix
  resolution: ambiguous suffix refused; last-12 resolves.
- `depends_on` on a todo item does nothing until queued (state semantics pinned).

## Open questions — resolved as proposed (2026-08-19 build)

1. Scheduled recurring items (Test Loop) and `depends_on`: NO — the webapp API refuses
   `depends_on` on schedule templates, and itersched clones strip the fields
   defensively (covers hand-edited queues). Cadence and gates don't mix.
2. A dependency on a `todo` item: allowed — satisfied is satisfied, however it got
   there. The dependent waits (visibly queued, blocked-by shown) for a human to queue
   the dep and for it to finish.
3. No depth/size guard — the cycle check alone. One addition beyond the letter of the
   spec, in its spirit: a dependency (or descendant) that is MISSING from both the open
   queue and the closed archive, and a DESCENDANT that closed failed, are treated like a
   failed dependency (flip to todo with the reason) — never a silent hang, never a
   silent run on a broken foundation.
