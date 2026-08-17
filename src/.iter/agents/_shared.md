# Shared instructions (all agents)

This file is appended to EVERY agent's context on every run — the store-once place
for rules that apply to all agents. Keep entries short and universal; agent-specific
guidance belongs in that agent's own file. (Files here starting with `_` are helpers,
never agent types.)

## Communicate clearly — a human must get it FAST

Goal: a human developer skimming your output understands the status, the
feedback, and any issues in seconds. Everything you write (outputs,
observations, work items, commit messages, docs) follows these rules:

- **Lead with a 1–2 sentence summary in simple words.** Put detailed
  explanations in a separate detail section BELOW the summary.
- **Use specific, common words.** No jargon, no invented terms, no implied
  meanings. Call files and things by their exact names (`testgroup.iter.md`,
  not "the manifest").
- **Describe — don't just state or name.** "the lock file was never deleted, so
  every later run waits forever" beats "stale lock issue".
- **Use an analogy when the concept is abstract.** One good comparison to an
  everyday thing speeds understanding more than a paragraph of precision.
- **Use bullet or numbered lists** whenever you enumerate things or explain a
  hierarchy. Numbers when order matters, bullets when it doesn't.
- **Avoid large blocks of dense text** — they slow human readers down. Break
  them up or cut them.

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

## Lock scope and codepath_ignore

Your work item's `codepath` is your lock scope: the directory tree you own for
this run. If the item carries `codepath_ignore` patterns (gitignore-style,
relative to the codepath), those subtrees are **carved out of your scope — do not
create, edit, or delete anything under them.** Another work item may own them and
be running there right now; the engine's lock lets you both through on exactly
that promise. Reading is fine anywhere. When you create work items, use the same
mechanism to parallelize: e.g. a code item on `<component>` with
`"codepath_ignore": ["$ITER_TEST_DIR/"]` alongside a testwriter item whose
codepath IS `<component>/$ITER_TEST_DIR` (the project's test directory name —
`globalsettings.test_dir`, exported as `$ITER_TEST_DIR`).

## Defect items carry their failing testgroup (red before fix)

A work item you create may sit queued for hours and then run against a tree that
has moved on. In the TDD flow that risk is handled by tests, not prose: a
defect-shaped item must carry the testgroup that proves the defect
(`source_testgroup` on sweep-born items; name the group in `mainwork` on items
you author). The receiving agent reproduces BEFORE fixing:

    "$ITER_BIN" runtests --project "$ITER_PROJECT" --group "<label>" --broken

`--broken` claims "the defect is still present". If the group is actually green
the claim is false: the engine flags the item (it fails at the turn boundary no
matter what you do next) — the item is STALE; touch no code and stop. When your
fix is done, gate completion with `--fixed` (claims "resolved"; any red or
script error flags the item). Plain `runtests` invocations are neutral — run
them freely while iterating; they never flag anything.

A defect claim that could have a test gets the test written first, then the fix
item. Only for genuinely untestable claims (external infra state, credentials)
may an item fall back to prose: state the claim, the check command, and
"if this no longer holds, report stale and stop" in `mainwork`.

## iter files — the FILENAME declares the role

Every `*.iter.md` file's NAME says what it IS: the word right before `.iter.md`
(any prefix is fine, e.g. `gateway.marker.iter.md`). The six roles, all singular:

- `*marker.iter.md` — a structure node (C4 object); frontmatter `level:` says which C4 level
- `*bizreq.iter.md` / `*techreq.iter.md` — requirement docs
- `*interface.iter.md` — an interface contract
- `*testgroup.iter.md` — test definitions (owned via the marker's `testgroup:` key)
- `*usecase.iter.md` — a use-case thread

Frontmatter supplies ATTRIBUTES, never identity: a stray `level:` key inside a
usecase file changes nothing — renaming the file is the only way to change its
role. A file matching no role is a plain context doc.

Any `*marker.iter.md` file you CREATE or TOUCH must begin with `---`-fenced
frontmatter; without it the node has no name/level on the Projects map.

- Structure node (`*marker.iter.md`, one per C4 object directory):

      ---
      name: "Human-Readable Name"
      level: component        # project | context | container | component
      description: "one line on what this C4 object is"
      uses: [interface-id]    # interfaces it consumes (optional)
      provides: [interface-id]# interfaces it serves (optional)
      testgroup: test/testgroup.iter.md   # THIS object's testgroup.iter.md (path relative to this marker file)
      test_dir: test          # subtree holding its test scripts (testwriter's lock scope)
      bizreq: bizreq.iter.md  # local requirement files (optional; default = beside the marker)
      techreq: techreq.iter.md
      ---

  **The marker file defines the C4 object — every file belonging to it is declared
  here, never inferred from directory positions.** `testgroup:` is MANDATORY for
  the test sweep: no key = this C4 object is deliberately outside the sweep (its
  tests never run automatically). If tests should exist, declare the key.

- Interface marker (`*.interface.iter.md`) — the LOGICAL data contract between
  C4 objects:

      ---
      interface: interface-id
      kind: json              # format family of the messages: json | xml | text | binary | library | …
      description: "one line on what data crosses this boundary"
      ---

  **The body IS the contract: the logical messages exchanged, shown as
  examples, never prose about them.** An interface is TRANSPORT-NEUTRAL — the
  same request/reply shapes may ride an HTTP body, a gRPC message, a queue
  record, argv/stdout, or an in-process struct, chained in any order, without
  renaming a field or changing a rule. Show each message as a fenced block
  holding a concrete or pseudo example with constraints as inline comments;
  add a strict-JSON worked-examples block (normative request → reply pairs)
  and at most a few invariant bullets for what examples cannot express. Model:
  `sample/greet-msg.interface.iter.md`. Tag a fence (```json) only when the
  block strictly parses as that language; pseudo-examples with ellipses stay
  untagged.

  **The two-clause test — the file is right when:** (1) a stranger could
  implement EITHER side from this file alone, and (2) nothing in it would
  change if a component were rebuilt in another language or deployed
  differently. Carrier bindings — routes, ports, topics, flags, exit codes,
  build and deploy facts — fail clause 2: record them on the marker of the C4
  object that serves that binding, never here.

  **WHAT, never WHO or HOW.** An interface is used by many C4 objects; the file
  must not name providers, consumers, or callers, nor describe how or where
  anyone uses it — the structure graph already carries that through marker
  `provides:`/`uses:` keys. Sentences like "consumed by X" go stale the moment a
  second consumer appears, and `iter validate` flags them.

  **Interfaces are shared contracts — reuse before creating.** Before adding a
  new interface, check the existing ids (every `*.interface.iter.md` in the tree
  — the Projects scan aggregates them globally) for a contract that already fits;
  extend or reference it instead of creating a near-duplicate. Create a new id
  only when no existing contract covers the need.

  **Copy the quotes** on the prose fields (`name`, `description`, `endpoint`). Prose
  routinely contains a colon-plus-space, and while the engine parses that fine
  unquoted, strict-YAML tools reading the same marker refuse the whole block. Bare
  single-token values (`level:`, `kind:`, `interface:`, the ids in `uses:`/
  `provides:`) stay unquoted.

  Every structure node's BODY must contain a `# Long Description` section: a
  plain-language description of the object for a non-technical reader — describe,
  don't state; no jargon; define every acronym on first use ("three letter
  acronym (TLA)"); link related project parts by their marker path. Write it for
  real when you create or substantially change a node — never leave `TBD`.

- Requirement docs, testgroup files, and plain context docs need no frontmatter.
  Stray keys in them are ignored — the filename already said what they are.

- **Project-WIDE requirements** live in the project reqs directory — `$ITER_REQS`
  in your environment (default `.iter/reqs/`; a project relocates it with a
  `reqs:` frontmatter key on its `level: project` marker). Component-local
  requirement files stay beside their component; a requirement that spans
  components belongs in `$ITER_REQS/bizreq.iter.md` / `techreq.iter.md`. The
  engine lists these files in your spin-up context automatically; work-item
  context/testfiles patterns can reference the directory as `{reqs}`.

If you encounter a marker with missing or malformed frontmatter anywhere in the
files you read for your work, correct it as part of your change and note the fix in
your output — ongoing verification is every agent's job. The deterministic check
is built in — after touching any iter file, run:

    "$ITER_BIN" validate --project "$ITER_PROJECT" --file <the file> --fix

`--fix` applies the safe corrections (fence normalization, quoting prose values
that contain ": "); everything else is reported for you to correct by hand.
Exit 0 = clean (info-level notes are fine); exit 1 = warn/error findings remain —
fix them before finishing.
