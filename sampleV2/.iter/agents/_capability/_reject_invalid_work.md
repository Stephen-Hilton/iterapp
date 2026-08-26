# Capability: reject invalid work (`iter reject`)

Failing an item means "I couldn't do the work" — the engine retries it. When the
problem is the WORK ITSELF (out of scope for the project, goal unclear, premise
no longer true, conflicts with a `*bizreq.iter.md` invariant), do not fail and
do not quietly complete. Reject it:

    "$ITER_BIN" reject --project "$ITER_PROJECT" --reason "<why, and what would make it acceptable>"

The engine moves the item to `todo` at the turn boundary — the human-review
bucket, where the user edits and requeues (or deletes) it. No retries are
burned; nothing gets buried in the completed archive. Your reason and your
output are what the re-evaluating human sees: name the blocking fact and the
smallest change that would make the item valid, then end your work.

## What a rejectable item looks like

- **Out of scope** for this project (a Netflix codebase asked to "order food").
- **Unclear in its goal** ("hit button" — no actor, no outcome).
- **Overly technical for its type** — e.g. a use-case item saying "run database
  query ABC", which is implementation, not a user journey.
- **In violation of a business invariant** in the global or local
  `*bizreq.iter.md` ("user logs in with a 1-character password").
- **Premise no longer true** — the thing the item asks you to change is already
  gone, or was decided the other way since the item was filed.

Do NOT mark rejected work complete, and do NOT grind out work you believe is
invalid. A rejection with a specific reason is a useful result; a completed item
that did the wrong thing is not.
