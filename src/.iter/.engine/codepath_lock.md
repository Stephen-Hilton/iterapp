# Engine procedure: codepath lock (acquire)

Engine-owned. This documents the hardcoded lock-acquisition behavior and the template
rendered into `.iter.lock` files. Do not edit expecting behavior to change — the
protocol lives in the engine; this file is its documentation and template.

## Procedure

Before a work item starts, the engine:

1. Scans for `.iter.lock` files in:
   - the work item's `codepath` itself,
   - every directory **below** it (recursive), and
   - every directory **above** it (ancestors to the filesystem root).
   Ancestors matter: a lock at `project/` must block work in `project/sub/`, and vice versa.
2. A lock file is **active** if it exists and `now < timeout`. Expired lock files are
   deleted on sight.
3. If any active lock is found: the record lock on the queue is released (work item back
   to `queued`) and the engine returns to Find Work.
4. If none: the engine writes `.iter.lock` into the codepath root using the template
   below, then proceeds.

## `.iter.lock` template

```json
{
  "workid": "{workid}",
  "agent": "{agent_type}",
  "pid": {engine_pid},
  "created": "{now_iso8601}",
  "timeout": "{now + max(codepath_lock_timeout_sec, agent max_work_timeout_sec)}"
}
```
