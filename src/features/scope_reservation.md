# scope_reservation — wide items drain their scope instead of starving

Plan only (2026-08-31) — nothing here is built. Target: the next major
rewrite (iter.v3).

## The starvation this fixes

Observed repeatedly with P1 plan trees:

1. A plan agent creates N code/testwriter children plus a final
   "verification" item (and follow-ups), verification depending on the
   children.
2. All children complete. Verification's codepath resolves to the code
   root — a whole-tree lock.
3. Unrelated work has started in the meantime. The picker skips
   verification every tick ("blocked by running scope"), but keeps
   admitting NEW unrelated items, because a waiting item holds nothing.
4. The tree is never simultaneously free → verification and its
   follow-ups never run. Livelock by admission.

Rejected shapes, for the record:

- **"Prework: drain all work" / logical engine restart** — needs
  stop/drain/start lifecycle machinery (who restarts? itersched fires
  during the stop? in-flight items?), and halts disjoint work a scoped
  drain would leave alone. For whole-tree it buys nothing over draining
  at admission; for anything narrower it is strictly worse.
- **"Whole-tree items consume ALL slots"** — right intent, wrong ledger.
  Slots don't gate `exec:shell` items (separate `max_shell_workers` cap,
  scheduler.rs:481), so shell work would still barge; and a not-running
  item occupying N slots muddies per-type caps, the tick summary, and the
  live `max_agents_at_NN` throttle. The place that already understands
  scope conflict is the pick gate — the fix belongs there.

## Design: reservation in pick_next

`pick_next` (scheduler.rs:727) already resolves every candidate's scopes,
skips candidates overlapping running scopes / on-disk locks, and records a
`Blocked{by, path, kind}` per skip. Add one pre-pass and one gate:

1. **Pre-pass — elect the reserver.** Best-`effective_priority()` item
   that is:
   - (a) a **barrier** item (marking below), AND
   - (b) dispatchable except for the scope gate: state queued,
     `eligible()`, deps `Satisfied`, not in the `deferred` backoff map,
     agent type discovered with `max_agent_count > 0`, AND
   - (c) currently scope-blocked.
   Its resolved codepaths become this tick's **reserved scopes**.

2. **Gate — respect the reservation.** In the candidate loop, skip any
   candidate (the reserver itself exempt) whose scopes overlap a reserved
   scope (existing `scopes_overlap`), UNLESS the candidate's
   `effective_priority()` is **strictly better** than the reserver's.
   Record `Blocked{kind: "reserved", by: <reserver workid>}` so
   blocked.json / the "free slots but N blocked" log line name the
   barrier instead of looking like a hung engine.

3. **Dispatch — unchanged.** Running items finish and release scopes;
   nothing new overlapping was admitted; on the first clean tick the
   reserver is the best runnable and the existing pick takes it.
   PathClaim, lock acquisition, `defer_after_conflict` untouched
   (acquire races become near-impossible once admission stops, so the
   escalating backoff can't bite the barrier item).

## Marking: automatic for whole-tree, explicit for wide

- **Automatic**: factor the detection out of `whole_tree_warning`
  (scheduler.rs:951) into a `locks_whole_tree()` predicate — any codepath
  resolving to the code root, without the `**` lockless carve-out.
  Whole-tree items are barriers with zero plan-agent / prompt changes,
  engine-enforced (prompts do not decide), and existing queues are fixed
  retroactively.
- **Explicit**: opt-in bool on `WorkItem` — proposed name `reserve`
  (serde `skip_serializing_if` false, like `depends_on_shallow`) — for
  scopes wide-but-not-whole-tree (e.g. verification over one big codedir
  that keeps losing to churn inside it). Name not final.

## The priority escape hatch is deliberate

Strictly-better priority still barges: a P0 hotfix must not queue behind
a P5 verification barrier. Equal-or-worse waits. Priorities are
lower-is-sooner (P0 most urgent, default 5), so a P1 plan's verification
child blocks default-P5 unrelated churn — the reported scenario — while
still yielding to genuine emergencies.

## Interaction with depends_on — the timing works out

While the verification item's deps are unsatisfied it fails condition
(b) and does NOT reserve, so unrelated work flows normally during the
build phase. The tick the last child closes complete, the reservation
switches on; in-flight work drains (bounded wait: the longest running
item), then verification runs. A wedged barrier (broken deps, parked
todo, agent type at 0) can never freeze the engine, because a
non-dispatchable item can't reserve.

## Edge cases

- **Two barriers**: the better-priority one reserves; the other is
  blocked by it like any candidate (whole-tree overlaps everything).
- **Shell items**: gated identically — the check lives in the shared
  candidate loop, not the slot layer.
- **Foreign / leftover `.iter.lock` files**: the reserver still waits on
  them via the existing ancestor-lock probe; expired ones are cleared on
  sight as today.

## Known limitation (defer)

The reservation is in-memory, engine-local: a second engine on the same
tree wouldn't respect it. If multi-engine matters, v2 is an on-disk
**intent lock** — write the `.iter.lock` with `pending: true`, which
`find_active_lock`/`find_ancestor_lock` treat as blocking for NEW
acquirers while the writer itself waits for the tree to truly clear.
`CodepathLockInfo` is `serde(default)` so the extra field is
forward-compatible with old engines (they'd ignore it — acceptable
during a mixed-version window).

## Visibility

- blocked.json entries with `kind: "reserved"` + reserver workid.
- Log on reservation start/stop: `barrier w-xxx reserving <scope>:
  waiting on N running item(s)`.
- Webapp: blocked-reason chip already renders from blocked.json; add the
  "reserved" kind label.

## Test plan

Alongside the existing picker tests (scheduler.rs:2214 area):

- worse-priority overlapping candidate skipped with kind `reserved`;
- strictly-better priority candidate barges;
- barrier with unsatisfied deps does not reserve;
- barrier dispatches on the first clean tick after the drain;
- explicit `reserve: true` on a subtree reserves only that subtree —
  disjoint work still admissible;
- whole-tree item with `codepath_ignore: ["**"]` (lockless read pass)
  does NOT auto-barrier.
