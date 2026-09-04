# Feature: the `question` state — agents ask the human, the human answers, the item queues itself

Status: MOCKED UP 2026-08-24 for review in `sampleV2/` (debug build, seeded
queue). Engine, API, CLI and webapp paths are implemented end to end; the
purpose of this round is feedback on the interface, not a release.

## The problem

Today an agent that hits a decision only Stephen can make has exactly one
escape hatch: create a TODO work item whose `mainwork` prose contains the
question. That works, but the round trip is hand-made:

- The question hides inside the request text, so nothing in the UI says "a
  human is being waited on" — a question looks like every other parked item.
- Answering means **Pause & Edit → retype the request so it now contains the
  answer → Queue**. The human edits the agent's prompt by hand, and the
  original question is overwritten in the process — the record of what was
  asked and what was decided is gone.
- Nothing distinguishes "held for review" (ToDo) from "blocked on a human
  decision" (a question). Both pile into the same bucket, and the blocking
  ones are the ones that should be loudest.

## Design

One new state, two new fields, one new CLI verb, one new API action.

### The state

    "state": "question"

- **Parked like `todo`:** never eligible for dispatch (`eligible()` returns
  false), fully editable, counted as open.
- **Queued when answered:** answering is the transition. That is the whole
  point of the state — an answered question is work that can run.
- **Loud:** its own color (teal), its own summary button and filter, and a sort
  order ABOVE ToDo — a question is a person blocking a machine, so it goes
  near the top of the list, not into the review pile.

### The two fields

    "question": "…what the agent needs decided, with its recommendations…"
    "answer":   "…what the human decided…"

Both are plain strings on the work item — one string each, not a thread of
turns. The webapp renders them as two separate blocks, so the question stays
readable after it is answered and the pair becomes the durable record of the
decision. `mainwork` is never rewritten to carry the answer; the engine injects
the Q&A into the prompt at run time (below).

Two timestamps ride along in `times`: `asked` and `answered`, so "how long has
this been waiting on me" is visible.

### How an agent raises a question

Two shapes, because two things happen:

1. **I cannot finish without an answer** — `iter ask`, which parks the CALLING
   work item:

       "$ITER_BIN" ask --project "$ITER_PROJECT" --question "<the question>"
       "$ITER_BIN" ask --project "$ITER_PROJECT" --file .iter/temp/q-<workid>.md

   Same file-flag mechanism as `iter reject`: the command writes
   `.iter/.engine/question-<workid>.txt`, and the engine moves the item to
   `question` at the turn boundary — no retries burned, partial output kept,
   nothing buried in the closed archive. The agent summarizes and ends its
   turn.

2. **Someone should decide this, but I can keep working** — a standalone
   question item:

       "$ITER_BIN" add --project "$ITER_PROJECT" --type plan \
         --title "Which storage backend for the ledger?" \
         --question "<the question>" --mainwork "<what to do once answered>"

   `--question` forces the new item's state to `question`. This is a
   deliberate exception to "the automation mode decides the state of created
   items" (workitem_automation.md): a question is a human gate by definition,
   so `automation: auto` does not queue it unanswered. The exception is
   printed, like the other state overrides.

### What a good question looks like (agent-side rules, `_shared.md`)

A question costs a human's attention, so the bar is high and the shape is
fixed. Before asking, the agent researches: the code node files
(`*.code.iter.md`), the `*.bizreq.iter.md` / `*.techreq.iter.md` under them,
the global context files, the interfaces and use-cases, and the actual code.
A question the repository already answers is not a question.

The `question` text is then written in four parts — for a reader who has NEVER
seen the codebase (revised 2026-09-04): the question is anchored in the business
flow first (the moment in the user's or operator's journey where the decision
bites, in their words), and every internal name — container, rule id, gate,
file, work item id — is glossed the first time it appears. Bare internal names
are what made questions unanswerable: the human could not find a starting point
and had to ask the agent to re-explain. See `_ask_the_human.md` for the
worked bad/good example. The parts:

1. **Where this comes up** — the point in the business flow where the decision
   is needed and why now, in plain sentences with internal names glossed.
2. **The decision** — the one thing being asked, stated as a question. One
   decision per work item.
3. **Recommendations** — two to four concrete options, each with what it buys
   and what it costs (effort, risk, what it forecloses). Cite the requirement
   or file each option honors or breaks.
4. **The agent's recommendation** — which option it would take and why, so an
   answer can be a single word.

### How the human answers

**In the row, with no extra hop.** Open a question item and the detail panel
shows the **Question** block and, directly under it, an editable **Answer**
box — the same box that holds the recorded answer once the item has run, so an
answer being written and an answer already given look like the one thing they
are. Nothing has to be opened and there is no Save button:

- **It saves itself.** Typing marks the draft dirty; a 5-second ticker flushes
  every dirty draft, and leaving the box flushes it immediately. A small status
  line beside the button says which state you are in (`unsaved — autosaves
  every 5s` → `saving…` → `saved`). Saving is a plain `PATCH` of the `answer`
  field: the item stays parked in `question`, so a half-formed reply can be
  left and picked up later without it running.
- **Answer and Queue** is the only button, and it means "this answer is
  finished": one POST stores it, stamps `times.answered`, and queues the item.

The box is live in the states where the server accepts an edit — `question`,
`todo`, `paused` — and is the read-only record everywhere else, so the UI never
offers an edit the API would refuse.

**In a lightbox, for a question worth reading properly.** The Actions menu's
first entry on any item that has an unanswered question is **Answer…**, and it
opens the question in a modal of its own: the question in its own scroll box
(44vh, so the answer area stays on screen under it), an answer box, and
**Close** / **Answer and Queue**. It is the same box in the sense that matters —
it carries the same `data-answer` attribute as the row's, so the drafts map, the
5-second autosave and the status line treat the two as one box, and typing in
either keeps the other in step. On an item whose state no longer accepts an
answer the same lightbox opens read-only: the question, the recorded answer,
Close.

The reason it exists is length. An agent's question runs to thousands of
characters, and reading one in the row means reading inside a list the engine
rewrites every time it writes to its database.

**Repaints are held while a lightbox is open, and while an answer box has
focus.** The engine's updates arrive over SSE and rebuild the rows wholesale,
which drops whatever element was scrolled and starts its replacement at the top.
Both holds work the same way: `render()` records that a paint is owed and
returns; closing the lightbox (or blurring the box) replays it immediately, so
the list catches up in one rebuild rather than staleness that waits for the next
event. Only the PAINT waits — `applyDelta()` keeps patching the data underneath
and `syncBodies()` keeps fetching row bodies, so nothing arrives late.

The server holds its end up too: an event whose `changed`, `removed` and
`closed` are all empty AND whose counts match the last event sent is not written
at all. The database moves on every agent heartbeat and log row; a row a reader
can see moves far less often, and only the second kind is worth an event.

The rest of the Actions menu carries the escapes: Queue without answering (the
human's call, not refused), Move to ToDo, Pause & Edit, Complete, Delete.

API:

    POST /api/workitems/<id>/action  {"action":"answer","answer":"…"}   → queued
    POST /api/workitems/<id>/action  {"action":"answer","answer":"…","queue":false}
    POST /api/workitems/<id>/action  {"action":"question"}              → park as a question
    PATCH /api/workitems/<id>        {"question":"…","answer":"…"}      (editable fields)

### What the agent sees when the item runs

The engine prepends an answered-question block to the **mainwork** turn — it is
composed, never stored in `mainwork`:

    # A question on this work item was answered

    Before this run, work on this item stopped to ask a human a question. The
    question and the answer are below — the answer is a decision, and it
    outranks any assumption in the request that follows.

    ## Asked
    <question>

    ## Answer
    <answer>

    Proceed on that answer. If it is ambiguous, or acting on it raises a NEW
    decision only a human can make, ask again with `iter ask` rather than
    guessing.

    ---
    <the original mainwork>

So the answer reaches the agent without anyone editing the request, and the
work item still reads as itself for every later human.

## What this is not

- **Not a chat.** One question, one answer, one string each. A follow-up is a
  new `iter ask` on the same item (the previous pair is replaced in the fields
  and preserved in the item's output).
- **Not a blocker on other work.** A question parks ONE item. Everything else
  in the queue keeps running; use `depends_on` if other items must wait for the
  answered one.
- **Not a notification system.** The webapp's Question count is the signal.
  Push/email is a later question of its own.

## Files touched

| File | Change |
|---|---|
| `src/workitems.rs` | `STATE_QUESTION`, `question` / `answer` fields, `times.asked` / `times.answered` |
| `src/main.rs` | `iter ask` subcommand; `iter add --question`; status counts |
| `src/scheduler.rs` | question flag path + `ask_item()` turn-boundary handler; answered-question prompt block; queue summary line |
| `src/server.rs` | `question` in counts and creatable states; `answer` / `question` actions; `question`+`answer` editable; patchable in the question state |
| `src/webapp/app.html` | state color/icon/order, summary button + filter, header chip with wait time, in-row Question block + autosaving Answer editor, the render hold that protects a caret mid-edit, form fields, mock seed data |
| `src/.iter/agents/_shared.md` | "Asking the human a question" section (research-first, four-part shape) |
| `tests/e2e.rs` | full round trip against the real binary: ask → park → answer → the answered run; plus the two `iter ask` refusals and the `--question` override |
| `src/scheduler.rs` (tests) | the composed prompt needs BOTH halves; `ask_item` refunds the attempt and clears a stale answer |

## Tests (per the house law: every guard proves it can fail)

- **E2E, the whole loop:** a stub agent runs `iter ask` → the item parks in
  `question` (not todo, not failed), attempts unburned, `times.asked` stamped,
  nothing closed. A second engine pass proves a parked question is inert. Then
  the answer goes on and the item queues: the next run's prompt contains the
  question, the answer, AND the original unrewritten request, and the item
  closes complete with the Q&A still on the archived record.
- **The refusals:** `iter ask` outside a work item exits 2 (no `$ITER_WORKID`);
  a one-line question with no options exits 2 (the research bar).
- **The override:** `iter add --question` under an `automation: auto` parent
  lands in `question`, not `queued`.
- **Unit:** the mainwork prompt is untouched with a question but no answer, an
  answer but no question, or neither — and with both, the decision precedes the
  request it governs. `ask_item` clears a previous round's answer, so a stale
  reply can never read as an answer to the new question.
- **API guards** (exercised live against sampleV2): an empty answer refuses,
  answering an item with no question refuses, `queue: false` saves the draft
  without dispatching.
- **The inline editor** (driven in a real browser against sampleV2): typing
  autosaves on the 5s tick without stealing focus, blur flushes at once, the
  state stays `question` until Answer and Queue, and a live SSE refresh landing
  mid-sentence leaves both the text and the caret position untouched.

## Open questions for this review round

1. **Sort order** — question items currently sort above ToDo and below Queued.
   Should they sort above everything, in-progress included?
2. **Force-queue** — queueing an unanswered question is allowed today (the
   human's call). Should it refuse instead?
3. **Standalone question items** (`iter add --question`) — worth keeping, or is
   `iter ask` (park the asking item) the only path worth having?
4. **A question that is answered "no, drop it"** — today the human deletes or
   completes the item. Is a dedicated "answered, no work needed" close-out
   worth a button?
5. **Timestamps** — `times.asked` / `times.answered` currently render as two
   extra rows at the end of the Time Records timeline, which is chronologically
   out of order for a mid-run ask. Fine, or should the timeline interleave?
