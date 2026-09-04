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

## Write for someone who has never seen this codebase

The person answering has NOT read the code, the node files, or this work item. They
know the business — what the product does for the people who use it — and nothing
about how the repository spells it. Every internal name (a container, a rule id, a
gate, a function, a file, a work item id) is a word they have never met. Used bare,
those names turn the question into noise, and the human's only move is to ask an
agent to re-explain it — the round trip your question was supposed to save.

So anchor the question in the business flow first: name the moment in the user's or
operator's journey where the decision bites, in the words a customer or operator
would use, and only then introduce the internal names, each glossed the first time
it appears. If the mechanism is genuinely complex, an analogy is welcome. Be concise
— context is not length.

Bad: "based on rule ABC, once _potatochip has traversed the cankor gate, should rule
XYZ apply only twice?"

Good: "When a customer asks for a bag of potato chips, we first confirm they have
paid (rule ABC, enforced by the payment container cntr_ABC) and that they have not
typed obscenities into the console (the 'cankor gate', a check run by the Rulebook
container). If they have typed obscenities, should we refuse the chips after one
offense or after two? The instructions in main.bizreq.iter.md do not say."

## Write the question in four parts, in this order

1. **Where this comes up** — the point in the business flow where the decision is
   needed and why it is needed now, written as the anchor above: plain sentences,
   internal names glossed as they appear. Assume the reader has never opened the
   file you are looking at.
2. **The question** — the one decision, stated as a question. One decision per
   work item; two questions are two items.
3. **Two to four options** — concrete, each with what it buys and what it costs
   (effort, risk, what it forecloses later), in the same plain terms. Name the
   requirement, file, or constraint each option honors or breaks, and say in a few
   words what that requirement is.
4. **Your recommendation** — which one you would take and why, so the human can
   answer in one word.

Never ask a question whose options you have not researched, and never ask one you
could answer by reading. An unhelpful question is a rejected question — and a
question the human has to send back for re-explanation is an unhelpful question.
