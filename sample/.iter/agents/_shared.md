# Shared instructions (all agents)

This file is appended to EVERY agent's context on every run — the store-once place
for rules that apply to all agents. Keep entries short and universal; agent-specific
guidance belongs in that agent's own file. (Files here starting with `_` are helpers,
never agent types.)

## Requesting a critical review

When your mainwork asks for a critical review (or "critique"), get one
synchronously — no work items involved — BEFORE acting on the reviewed result
(e.g. a plan agent reviews its plan before creating the follow-on items):

1. Write the material to review (the plan text, a change summary plus file list,
   etc.) to a temp file, e.g. `.iter/temp/critique-<workid>.md`.
2. Run — and set the Bash tool call's timeout high (up to 1800000 ms); the review
   takes minutes:

       "$ITER_BIN" critreview --project "$ITER_PROJECT" --file <material.md> --context <requirements.md> ...

3. The critic's verdict and numbered feedback arrive on stdout. Triage it
   yourself: decide which items are valid given the requirements, do a
   cost/benefit pass on the valid ones, implement what is worth doing, and record
   each item's disposition in your output.
4. One more review after major revisions is fine; never run more than two per
   work item.

Exit codes: **0** — feedback on stdout, triage it. **Any nonzero exit** — the
review could not be delivered and your work item has already been flagged to
fail (the engine enforces this at the next turn boundary regardless of what you
do). STOP immediately: do not create work items, do not proceed without the
review, end your session stating the critreview failure. A requested review is
part of the work — work without it is not done.

## Premise stamp on work items that assert current state

A work item you create may sit queued for hours and then run against a tree that
has moved on — prose that was true when written can be false at dispatch, and the
receiving agent cannot tell from the text alone. When an item's value depends on a
claim about current state ("X is broken", "Y is missing", "Z mismatches"), end its
`mainwork` with a premise stamp and put `premise-check` first in its `prework`:

    PREMISE (re-verify before mainwork):
    - authored-at: <git rev-parse --short HEAD> on <branch>, <UTC timestamp>
    - holds-if: <shell command>   # expected: <exact output or exit code>

One to three `holds-if` lines, each a cheap command that succeeds ONLY while the
claimed state still exists — if someone fixes it, the command must stop matching.
The best holds-if is a FAILING TEST: name the test invocation and expect failure.
That one line is then the premise at dispatch, the acceptance criterion during the
work, and a permanent regression test once it passes. A command that passes either
way (a file existing, a service answering) proves nothing.

The step file `.iter/prepostwork/premise-check.md` tells the receiving agent what
to do when a line fails: report the premise stale and stop, rather than build on a
dead assumption.

## Marker file frontmatter — verify on contact

Any `*.iter.md` marker you CREATE or TOUCH must begin with `---`-fenced frontmatter;
a marker without it is invisible to the Projects structure view.

- Structure node (one per component directory):

      ---
      name: "Human-Readable Component Name"
      level: component        # project | context | container | component
      description: "one line on what this component is"
      uses: [interface-id]    # interfaces it consumes (optional)
      provides: [interface-id]# interfaces it serves (optional)
      ---

- Interface marker (`*.interface.iter.md`):

      ---
      interface: interface-id
      kind: http              # http | grpc | kafka | sql | file | cli | library | …
      endpoint: "POST /v1/example"
      description: "one line on the contract"
      ---

  **Copy the quotes** on the prose fields (`name`, `description`, `endpoint`). Prose
  routinely contains a colon-plus-space, and while the engine parses that fine
  unquoted, strict-YAML tools reading the same marker refuse the whole block. Bare
  single-token values (`level:`, `kind:`, `interface:`, the ids in `uses:`/
  `provides:`) stay unquoted.

- Requirement/context markers (`*.bizreq.iter.md`, `*.techreq.iter.md`, plain
  context docs) need no frontmatter. Test-group manifests (`testgroups.iter.md`)
  need none either and must never be given `level:` — they are found by filename
  and are not structure nodes.

If you encounter a marker with missing or malformed frontmatter anywhere in the
files you read for your work, correct it as part of your change and note the fix in
your output — ongoing verification is every agent's job.
