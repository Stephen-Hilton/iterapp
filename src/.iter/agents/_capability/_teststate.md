# Capability: the Test Loop gate (`iter teststate`)

`teststate:` is a frontmatter flag on a node, use-case, or interface file that says
whether the Test Loop (the engine's scheduled test sweep) runs that object's
testgroups. It exists for use-case-centric TDD: the user parks broad subtrees out of
the sweep, and each new use-case pulls exactly its own dependencies back in, so the
loop works on what is being built rather than on everything that exists.

Four values, one selector:

- `inherit` — the default. The nearest ancestor with a flag decides.
- `omit` — park this object out of the sweep. Carries down its whole subtree.
- `include` — re-enter this object. Works under an omitted ancestor; **refused**
  under a blocked one.
- `block` — hard park: this object needs outside or vendor setup that is not
  present, so its tests cannot meaningfully run. Agent-proof by design.

**The nearest flag wins**, so including a component works even under an omitted
container.

## Editing it — the engine-owned write path

Never hand-edit the `teststate:` key. Use the command, which works from any agent
regardless of lock scope:

    "$ITER_BIN" teststate --project "$ITER_PROJECT" --include "<ref>"
    "$ITER_BIN" teststate --project "$ITER_PROJECT" --list

A `<ref>` is a node key, a name, a use-case name, an interface id, or a
declaring-file path suffix. `--omit`, `--include`, `--block` and `--clear` all
repeat; `--clear` removes the object's own flag so ancestors and the default apply
again. `--list` prints every object with its own flag and its EFFECTIVE state.

## The refusal is the design

If the command REFUSES because a node is `teststate: block`, do NOT try to force it
or work around it — that refusal is the whole point of `block`. Report the blocked
object in your output so the user decides.

Unless your work item explicitly asks for it, only ever `--include`. Parking things
(`--omit` / `--block`) and un-parking them (`--clear`) are the user's calls, not an
agent's.

## When an agent includes something

An object cannot be included before it exists: it enters the sweep the moment its
declaring file exists and is linked. So the rule for a build handoff is that each
built node gets re-entered into the Test Loop when its code item completes — links
and sweep coverage must reflect what was BUILT, not what was proposed.
