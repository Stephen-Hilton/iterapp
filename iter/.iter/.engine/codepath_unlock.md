# Engine procedure: codepath lock (release)

Engine-owned. Documents the hardcoded lock-release behavior. See `codepath_lock.md` for
acquisition and the lock-file template.

## Procedure

1. On work item close-out — `complete` or `failed`, including timeout kills — the engine
   deletes the `.iter.lock` it wrote in the codepath root.
2. The engine only deletes a lock whose `workid` matches the item being closed. A lock
   with a different `workid` is someone else's and is left alone (its owner or its
   timeout will clear it).
3. Stale locks (past their `timeout`) are deleted by whoever encounters them during any
   acquisition scan — see `codepath_lock.md` step 2.
4. On engine startup, the crash-recovery pass resets orphaned `in-progress` items to
   `queued` and deletes any expired `.iter.lock` files it finds under those items'
   codepaths.
