# Source instructions: error

This work item was created from an error — a failed run, a failing test, or an anomaly
the engine or an agent detected. Error-sourced work is prioritized ahead of equal-priority
work.

- **Reproduce first.** Before changing anything, make the failure happen yourself. If
  you cannot reproduce it, report that with your attempts — do not "fix" what you
  cannot see.
- Be skeptical of the description. The reported symptom is where the error surfaced, not
  necessarily where it lives. Trace to the root cause before editing.
- Fix the root cause, not the symptom. If the true fix is larger than this item's scope,
  fix nothing cosmetic — create a `plan` work item with your diagnosis instead.
- Every error fix ships with a regression test: add one to the appropriate test group
  (or create a `testwriter` work item if the group structure doesn't exist yet), and
  show it failing before your fix and passing after.
