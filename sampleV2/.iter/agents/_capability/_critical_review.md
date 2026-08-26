# Capability: request a critical review (`iter critreview`)

When your mainwork asks for a critical review (or "critique"), get one
synchronously — no work items involved — BEFORE acting on the reviewed result
(e.g. a plan agent reviews its plan before creating the follow-on items):

1. Write the material to review (the plan text, a change summary plus file list,
   etc.) to a temp file, e.g. `$ITER_TEMP/critique-<workid>.md`. (`$ITER_TEMP` is
   the absolute scratch directory the engine exports into your environment; never
   write a relative `.iter/temp/...` path, which resolves against your working
   directory and mints a stray temp tree there. Files whose name starts with
   `critique-` are kept by the temp sweeper rather than aged out.)
2. Run — and set the Bash tool call's timeout high (up to 1800000 ms); the review
   takes minutes:

       "$ITER_BIN" critreview --project "$ITER_PROJECT" --file <material.md> --context <requirements.md> ...

   `--context` repeats: give the critic every requirements file it must judge
   against, or it will judge against taste.
3. The critic's verdict and numbered feedback arrive on stdout. Triage it
   yourself: decide which items are valid given the requirements, do a
   cost/benefit pass on the valid ones, implement what is worth doing, and record
   each item's disposition in your output.
4. After major revisions, request another review of the revised material. There is
   a cap on review rounds per work item — the shared instructions' index line for
   this capability states the live number (Settings → `critreview_max_rounds`);
   obey that number, not a remembered one. Stop earlier the moment a review comes
   back with no material findings — rounds are a budget, not a target.

Exit codes: **0** — feedback on stdout, triage it. **Any nonzero exit** — the
review could not be delivered and your work item has already been flagged to
fail (the engine enforces this at the next turn boundary regardless of what you
do). STOP immediately: do not create work items, do not proceed without the
review, end your session stating the critreview failure. A requested review is
part of the work — work without it is not done.
