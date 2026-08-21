# Engine-control step: iterloop-wait-for-stop

Signal the iterloop engine to **drain and stop**: pick up no new work, let every
in-flight work item run to completion, then shut down.

1. Write a file named `stop.signal` into the project's `.iter/.engine/` directory,
   containing a single line: the current ISO-8601 UTC timestamp, the word `drain`, and
   the reason, e.g. `2026-08-11T09:00:00Z drain requested by workitem {workid}`.
   (The `drain` token is what distinguishes this from an immediate stop.)
2. Confirm the file exists.
3. Do nothing else — do not touch project code in this step.
4. Report `DRAIN-SIGNAL-WRITTEN` and the reason.
