# Engine-control step: iterloop-stop

Signal the iterloop engine to stop at the next tick boundary. In-flight work items
(including yours) finish their current prompt turn, but the engine starts nothing new
and shuts down as soon as its bookkeeping allows.

1. Write a file named `stop.signal` into the project's `.iter/.engine/` directory,
   containing a single line: the current ISO-8601 UTC timestamp and the reason, e.g.
   `2026-08-11T09:00:00Z requested by workitem {workid}`.
2. Confirm the file exists.
3. Do nothing else — do not touch project code in this step.
4. Report `STOP-SIGNAL-WRITTEN` and the reason.
