# Capability: create a work item (`iter add`)

Read this file before you file a work item, every time — not from memory of what it
probably says. Creating an item is how one agent hands work to another: you describe
the work, the engine queues it, and some other agent (or the same type, later) picks
it up with none of your context except what you wrote down.

## The command

    "$ITER_BIN" add --project "$ITER_PROJECT" --file <item.json>

`$ITER_BIN` is the absolute path of the running iter executable and `$ITER_PROJECT`
is the project root that owns the work queue — the engine sets both in your
environment, so this command works from any codepath. Write `<item.json>` into
`$ITER_TEMP/` (see "Scratch files" in the shared instructions); a relative path
mints a stray temp directory wherever your working directory happens to be.

The same fields are available as flags for a one-liner, which is easier when the item
is short:

    "$ITER_BIN" add --project "$ITER_PROJECT" --type plan --priority 3 \
      --title "plan: build out C4 objects for usecase <name>" \
      --mainwork "<where, what, why — the three-tier request text>"

`iter add` prints `added <workid> …` on success. Capture that workid — it is what
`--depends-on` takes.

## The JSON shape

Every key is optional except `type`, `title`, and `mainwork`. This is the full set an
agent may set:

    {
      "type": "code",                       // the target agent type (see below)
      "title": "short line a human scans in a list",
      "mainwork": "the request text — three tiers, see below",
      "codepath": "/abs/or/relative/dir",   // the item's lock scope; narrowest that owns the work
      "codepaths": [],                      // extra directories to lock, when the node declares several codedirs
      "codepath_ignore": ["test/"],         // gitignore-style subtrees carved OUT of that lock
      "priority": 5,                        // 0–10, LOWER = sooner (P0 most urgent, default 5)
      "risk": 5,                            // 0–10
      "source": "agent: code",              // "agent: <your agent type>"
      "source_testgroup": "<label>",        // provenance: the testgroup this item exists to turn green
      "source_tests": ["test02"],           // which tests were red when the item was born (informational)
      "depends_on": ["<workid>"],           // ordering gate — see below
      "depends_on_shallow": false,          // wait for the named items only, not their descendants
      "automation": "auto",                 // how items THIS item creates are born; usually leave unset
      "model": "sonnet",                    // per-item model override — see below; omit when unsure
      "question": "",                       // raise the item AS a question instead of work to run
      "context": ["<file>"],                // files the receiving agent must read first
      "testfiles": ["<file>"],              // test files, for test-type items
      "prework": [], "postwork": []         // named prepostwork steps from .iter/prepostwork/
    }

**Never set these** — they are the engine's to write, and anything you put there is
ignored or overridden: `workid`, `state`, `created_by`, `attempts`, `output`,
`lasterror`, `times`, `git_start_commit`, `exec`, `sched`, `source_schedule`.
`iter add` also cannot create a `scheduled` item — schedules are user-created in the
webapp and the command refuses.

## Work items you create: never set `state`

Do not set `state` on work items you create. The engine derives it from YOUR work
item's automation mode, inherited down the whole chain from the original request:
`automation: review` → your items are born `todo` (a human reviews each stage before
it runs); `automation: auto` → born `queued` (fully automated build). A user-filed
item that named no mode of its own takes `globalsettings.default_automation`
(Settings). Any `todo`/`queued` you write is overridden — the mode, not the prompt,
decides. Design every handoff to work in BOTH modes: the documents and mainwork must
stand alone whether a human reads them first or an agent picks them up seconds later.
(Guards outrank automation: `iter reject`, the non-convergence guard, and failed
dependencies land items in `todo` in any mode, and `--question` lands an item in
`question` in any mode.)

You will rarely set `automation` yourself. Setting it changes how the items your NEW
item creates are born, not how your new item is born.

## Authoring `mainwork` (request) text on items you create

Each item's `mainwork` is read twice: by a human deciding whether the item should
run, and by the agent that runs it. Author it in the same three-tier shape as your
outputs:

1. Open with a few plain-language sentences: where in the codebase the item
   operates, what must change, and why — which requirement or test of the
   current mainwork it serves.
2. Then the specifics as hierarchical bullets — acceptance criteria, files,
   constraints — one line each, two max.
3. Put agent-only detail (exact commands, ids, raw listings the human should
   not wade through) at the bottom, clearly last.

## `codepath` — the lock scope you are handing over

`codepath` is the directory tree the receiving item owns for its run; the engine
locks it. Narrower = more parallelism. Use `codepath_ignore` to carve subtrees back
out so two items can run at once: e.g. a `code` item on `<component>` with
`"codepath_ignore": ["$ITER_TEST_DIR/"]` alongside a `testwriter` item whose codepath
IS `<component>/$ITER_TEST_DIR`. The test directory name comes from
`globalsettings.test_dir` (exported as `$ITER_TEST_DIR`); never guess it. The engine
also enforces code/testwriter scope disjointness deterministically — but write it
correctly anyway.

## `depends_on` — real ordering constraints, declared not staged

Steps with no ordering constraint get no `depends_on`: they compete on priority and
run in parallel. Declare a dependency only when step B builds on what step A
produces (A makes the tree compile, B adds code to it; A relocates a module, B
imports it from the new home).

- Create A first, capture the workid `iter add` prints, then create B with
  `--depends-on <that workid>` (or `"depends_on": ["<workid>"]`). Chains and fan-ins
  are fine — `--depends-on` repeats, and B may wait on several items.
- Dependencies must NAME EXISTING ITEMS: `iter add` resolves them against the queue
  at add time and refuses unknown ids (exit 2) — which is why a batch is created in
  dependency order. A workid or any unambiguous suffix works; the convention is the
  last 12 characters, what the webapp shows.
- A dependency is satisfied only when the item closes complete AND everything it
  created is closed complete, transitively — so depending on another PLAN item means
  "after everything that plan spawns finishes", which is usually what you want.
  `depends_on_shallow` opts out: wait for the named items' own completion only.
- A gated item never dispatches until every dependency is satisfied; a FAILED
  dependency flips the dependent to `todo` for human review. Ambiguous, unknown, or
  cyclic dependencies refuse (exit 2).
- `depends_on` composes with review-mode gating: deps on a `todo` item are declared
  but dormant — the gate applies from the moment the item is queued. A queued item
  with unmet dependencies is safe: it stays visibly queued and blocked until they
  close complete.

Never hold later slices back to create "when the first wave finishes", and never
build a plan that needs you (or a human) to sequence waves by hand. Create the whole
batch, in dependency order, in one pass.

## `model` — which model the item's agent runs on

`model` overrides the agent type's default model for this one item, at dispatch. The
valid values are `opus`, `sonnet`, `haiku`, `fable`; `iter add` refuses anything else
with exit 2 naming the valid values. The point is to spend the expensive models where
judgment lives and run mechanical work cheap.

- **Simple, well-specified, mechanical work → `"sonnet"`.** The work is fully
  described, there is one obviously right answer, and doing it is typing rather than
  deciding: comment sweeps, repointing documentation at moved files, plumbing a
  rename through the call sites, adding a field that already has a stated shape.
- **Complex work, or fuzzy requirements → `"fable"`.** The item needs the agent to
  weigh options, discover what the requirements actually imply, or design something
  that will be lived with: anything where a wrong-but-plausible answer costs more
  than the run does.
- **Unsure → omit the field.** The agent type's own default then applies, which is
  the tuned setting. Omitting is always safe; guessing wrong is not.

A plan agent filing a programme sets this per child, because it is the one agent that
knows which slices are typing and which are thinking.

## `source_testgroup` — carry the provenance

When you create an item because a testgroup is red — or you are escalating an item
that itself carried a testgroup — put the label on the new item
(`--source-testgroup "<label>"`). Three things depend on it: the sweep's dedup guard
(one open item per group), the webapp's run-history → work item link, and the
engine's non-convergence guard, which counts the loop's laps — the third plan born
from the same testgroup is held in `todo` for human review instead of running.

Carry `source_tests` too when you have it.

## Defect-shaped items carry a test, not prose

A work item you create may sit queued for hours and then run against a tree that has
moved on. A defect claim that could have a test gets the test written first, then the
fix item; name the failing testgroup in `mainwork` so the receiving agent can
reproduce before fixing (see `_runtests.md`). Only for genuinely untestable claims
(external infrastructure state, credentials) may an item fall back to prose: state
the claim, the check command, and "if this no longer holds, report stale and stop" in
`mainwork`.

## Raising a question instead of work

`--question "<the question>"` (JSON `"question"`) files the item in the `question`
state whatever the automation mode says: the text is a decision needed from a human,
and the item queues itself for work once someone answers it in the webapp. Use it for
a decision that does NOT block you right now — keep working and let the answer arrive
later. When the decision DOES block your current item, use `iter ask` instead
(`_ask_the_human.md`). Either way, write the question in the four-part format that
file describes.

## When the add refuses

If the add refuses because the queue is full (`max_open_workitems`), report the
refused items in your output instead of retrying. Same for an exit 2 on an unknown
dependency or an invalid model — fix the call if you can, otherwise say what refused
and why in your output.
