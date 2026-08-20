# Feature: Test-Loop gate (`test_loop:` flag)

Status: BUILT 2026-08-20 — park C4 objects, use-cases, and interfaces out of
the deterministic test sweep without removing anything, so attention can focus
on a few features at a time (use-case-centric TDD) and vendor-blocked objects
can't generate confusion and bad code.

## The flag

One frontmatter key on the declaring file (marker / usecase / interface —
attributes live on the object, never in a central config list):

    test_loop: omit      # workflow parking — agents may re-include
    test_loop: include   # re-enter a subtree under an omitted ancestor
    test_loop: blocked   # hard park (vendor/outside setup missing) — agent-proof

## Resolution (three lines)

1. `blocked` anywhere on the ancestor chain (self included) → OUT, period; no
   descendant `include` overrides it.
2. Otherwise the NEAREST explicit omit/include walking up from the object
   decides — so omitting a container carries down to every component, and an
   `include` on one component surgically re-enters it.
3. No flag anywhere → included (today's behavior; the feature is invisible
   until used).

Ancestry is directory nesting (node keys), the same derivation the Projects
tree renders. Use-cases and interfaces are global objects the hierarchy
doesn't own: OWN FLAG ONLY, no ancestry, and `include` is a no-op there
(validate says so).

## What omission does

- The sweep skips the object's groups entirely — no runs, no fix items, and
  (for parked use-cases/interfaces) the missing-`testgroup:`-key authoring
  machinery is suspended too: parked means "not spending attention here yet".
- UNSTARTED sweep-born items for a parked group auto-close (the existing
  green-stale-close mechanic, different reason) so parked work stops burning
  agent time. Started items are left alone.
- Nothing is silent: the sweep summary counts omissions, notes name each one,
  `/api/testgroups` rows carry the state, and the webapp shows ⛔ badges in
  the Projects tree and the Testing lightbox.
- Lifting the flag reverses everything: the next sweep re-runs the groups and
  re-births fix items if they are still red.

`testgroup: none` remains the PERMANENT "deliberately untested" opt-out;
`test_loop: omit` is the temporary "not yet". Different intents, different keys.

## Editing surface

- `iter testloop --omit/--include/--block/--clear <ref>` (repeatable;
  `--list` prints flag → effective state for everything). Ref = node key,
  name, use-case name, interface id, or declaring-file path suffix; ambiguous
  or unknown refs refuse (exit 2), never guess. The engine-owned write path —
  usable from any agent regardless of lock scope, like `iter usecase`.
- `POST /api/testloop {target, action}` — the webapp's toggle buttons
  (Projects tree node/use-case/interface panels), same rules, refusals → 409.
- The `blocked` contract: `--include`, `--omit`, and `--clear` all REFUSE to
  touch a blocked flag (and `--include` refuses under a blocked ancestor,
  where it could never take effect). Only a human editing the marker file
  lifts a block. This is what makes the usecase agent's auto-include safe.

## Use-case-centric TDD (the workflow this serves)

1. Park the top: `iter testloop --omit <context/container>` for each subtree.
2. The usecase agent builds a use case and runs `iter testloop --include` on
   every PRESENT participant (its instructions require this; blocked refusals
   are reported, never forced). Missing objects get included by the plan flow
   when they are built — a marker that doesn't exist yet can't be flagged.
3. The sweep now tests only the use case and its dependencies.
4. Green → next use case; includes accumulate, coverage grows monotonically.

## Tests (per the house law: every guard proves it can fail)

- Resolver: nearest-wins (include under omitted ancestor), blocked beats a
  descendant include, root-key ("") ancestry, own-flag-only for use-cases.
- testloop_apply: include refused on blocked self AND blocked ancestor; clear
  refused on blocked; ambiguous/unknown refs refuse; frontmatter edit
  round-trips (set, replace, clear) preserving other lines.
- Sweep: an omitted container's component group is skipped and counted while
  a sibling still runs; an `include` child under an omitted parent runs; a
  parked group's unstarted sweep item auto-closes.
- E2E: `iter testloop` omit → sweep skips (summary says so) → include →
  sweep runs it again; blocked include refusal exits 2.
