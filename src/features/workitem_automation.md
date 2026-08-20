# Feature: Automation mode (`automation: review | auto`)

Status: BUILT 2026-08-20 — whether agent-created work items are born `todo`
(human gate per stage) or `queued` (fully automated build) is decided by ONE
field on the originating request and enforced by the engine, ending the era of
prompt-hard-coded `"state": "todo"` instructions that overwrote the user's
intent (or collided with it).

## The problem

Agent prompts (plan, usecase) dictated the state of the items they created —
usually `todo`. The user's initial request couldn't say "run this fully
automated": the instruction was re-transmitted (or not) through every hop of
the handoff chain, so intent got overwritten at best and caused conflicting
instructions at worst. Too many TODOs, no reliable way to choose.

## Design

One optional field on a work item:

    "automation": "review" | "auto"     (unset = inherit / review)

- **Meaning:** how items THIS item creates are born. `review` → children land
  `todo`; `auto` → children land `queued`. It describes the lineage's gating,
  not the item's own state.
- **Inheritance:** unset inherits the creating parent's mode at `iter add`
  time (the engine knows the parent from `$ITER_WORKID`, same lookup as
  `created_by`); the resolved value is STORED on the child so grandchildren
  inherit transitively. No parent (user-created) → review.
- **Engine-enforced derivation:** on agent-sourced adds (`$ITER_WORKID`
  present) the child's `todo`/`queued` state is DERIVED from the effective
  mode, overriding whatever the prompt wrote (the override is printed, so it
  is visible in the agent transcript). An explicit `paused` survives;
  `scheduled` was already refused on this path. **Prompts do not decide
  state** — `_shared.md` now says exactly that, and the per-agent
  "create in state todo" instructions are deleted.
- **User-created items are never overridden**: the webapp form's save-state
  selector still rules the item's own state; the new Automation selector on
  the form sets the field that steers descendants. CLI: `iter add
  --automation review|auto`. A typo'd value refuses (exit 2 / HTTP 400) —
  silently meaning "review" would be the wrong failure mode.
- **Guards outrank automation in BOTH modes:** `iter reject`, the
  non-convergence guard, and failed-dependency flips still land items in
  `todo` — those are "a human must look" outcomes, not workflow gating. The
  sweep keeps its own gates too (per-testgroup `auto_fix`, always-todo
  authoring items) — the sweep is not part of any request lineage.

## Composition

`automation: auto` + `depends_on` = queue an entire dependency-ordered batch
from one request and let the engine sequence it unattended — the complete
no-babysitting version of the overnight-waves scenario
(workitem_dependency.md). Review mode composes too: dependencies declared on a
todo item are dormant until a human queues it.

## Tests (per the house law: every guard proves it can fail)

- E2E: under an auto parent a prompt-written `"state": "todo"` is overridden
  to queued (and the note printed); under a review-default parent the child
  lands todo; a grandchild of the auto lineage inherits auto transitively;
  user adds are untouched; an invalid mode refuses exit 2.
- The lifecycle e2e's handoff seed carries `automation: auto` — under the
  review default its handoff child would gate in todo and the drain would
  hang, which is itself the proof the gate works.
