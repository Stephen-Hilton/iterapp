# Capability: ask the human a question (`iter ask`)

Some decisions are not yours to make: what the product should do, which trade-off the
project wants to live with, which of two defensible designs to carry for years.
Guessing one of those and building on it is worse than stopping. Ask:

    "$ITER_BIN" ask --project "$ITER_PROJECT" --question "<the question>"
    "$ITER_BIN" ask --project "$ITER_PROJECT" --file $ITER_TEMP/q-<workid>.md

(Use `--file` for anything multi-paragraph — which the four-part format below almost
always is. `$ITER_TEMP` is the absolute scratch directory the engine exports; never
write a relative `.iter/temp/...` path.)

Your work item moves to the `question` state at the turn boundary — parked, no
retries burned, your turns so far kept as the research behind the ask. When a human
answers it in the webapp, the item queues again and the agent that picks it up gets
the question and the answer at the top of its request. Summarize the question in your
output and end your turn; do no further work on that item.

If the decision does NOT block you, raise it as its own item instead and keep going:

    "$ITER_BIN" add --type <agent> --title "<the decision, as a question>" \
      --question "<the question>" --mainwork "<what to do once it is answered>"

## Research before you ask

A question the repository already answers wastes the one resource an agent cannot
make more of. Read the relevant `*.code.iter.md` node files, the `*.bizreq.iter.md` /
`*.techreq.iter.md` under them, the global context files, the interface contracts and
use-cases, and the actual code. Ask only what none of them decide.

## Write the question in four parts, in this order

1. **Context** — where in the codebase this sits and why the decision is needed
   now. Plain sentences; assume the reader has not seen this work item.
2. **The question** — the one decision, stated as a question. One decision per
   work item; two questions are two items.
3. **Two to four options** — concrete, each with what it buys and what it costs
   (effort, risk, what it forecloses later). Name the requirement, file, or
   constraint each option honors or breaks.
4. **Your recommendation** — which one you would take and why, so the human can
   answer in one word.

Never ask a question whose options you have not researched, and never ask one you
could answer by reading. An unhelpful question is a rejected question.
