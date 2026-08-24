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
4. After major revisions, request another review of the revised material. Cap:
   at most {critreview_max_rounds} review round(s) per work item (Settings →
   `critreview_max_rounds`); stop earlier the moment a review comes back with
   no material findings — rounds are a budget, not a target.

Exit codes: **0** — feedback on stdout, triage it. **Any nonzero exit** — the
review could not be delivered and your work item has already been flagged to
fail (the engine enforces this at the next turn boundary regardless of what you
do). STOP immediately: do not create work items, do not proceed without the
review, end your session stating the critreview failure. A requested review is
part of the work — work without it is not done.

## Work items you create: never set `state`

Do not set `state` on work items you create (`iter add`). The engine derives it
from YOUR work item's automation mode, inherited down the whole chain from the
original request: `automation: review` → your items are born `todo` (a human
reviews each stage before it runs); `automation: auto` → born `queued` (fully
automated build). Any `todo`/`queued` you write is overridden — the mode, not
the prompt, decides. Design every handoff to work in BOTH modes: the documents
and mainwork must stand alone whether a human reads them first or an agent
picks them up seconds later. (Guards outrank automation: `iter reject`, the
non-convergence guard, and failed dependencies land items in `todo` in any
mode.)

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

## Rejecting invalid work (any agent)

Failing an item means "I couldn't do the work" — the engine retries it. When the
problem is the WORK ITSELF (out of scope for the project, goal unclear, premise
no longer true, conflicts with a `*bizreq.iter.md` invariant), do not fail and
do not quietly complete. Reject it:

    "$ITER_BIN" reject --project "$ITER_PROJECT" --reason "<why, and what would make it acceptable>"

The engine moves the item to `todo` at the turn boundary — the human-review
bucket, where the user edits and requeues (or deletes) it. No retries are
burned; nothing gets buried in the completed archive. Your reason and your
output are what the re-evaluating human sees: name the blocking fact and the
smallest change that would make the item valid, then end your work.

## iter files — the FILENAME declares the nodetype (structureV2)

Every `*.iter.md` file's NAME says what it IS, via the explicit DOT RULE:
`*.nodetype.iter.md` — the nodetype segment must be preceded by a dot unless
the file has no prefix at all, lowercase, case-sensitively. Valid:
`gateway.code.iter.md`, `code.iter.md`. Invalid: `gateway_code.iter.md`,
`gateway.Code.iter.md`. The seven nodetypes:

- `*.main.iter.md` — THE project head (one per project; `$ITER_MAINFILE`)
- `*.code.iter.md` — a code node (C4 object); frontmatter `level:` says which level
- `*.bizreq.iter.md` / `*.techreq.iter.md` — requirement docs
- `*.interface.iter.md` — an interface contract (global object)
- `*.testgroup.iter.md` — test definitions (linked via a node's `children.testgroups`)
- `*.usecase.iter.md` — a use-case thread (global object)

Frontmatter supplies ATTRIBUTES, never identity: a stray `level:` key inside a
usecase file changes nothing — renaming the file is the only way to change its
nodetype. A file matching no nodetype is a plain context doc.

**Nodes join the structure ONLY through explicit `children` links** — paths or
globs in frontmatter; directory nesting alone links NOTHING. A node whose
declared glob (e.g. `{thisfiledir}/test/*.testgroup.iter.md`) later matches a
new file picks it up automatically — that is the declared link doing the work.
Files matching the naming rules but linked from nowhere land in the ORPHANAGE
(visible in `iter markers` output and the webapp) — check it if something you
created is not appearing in the tree.

Paths in links use `{placeholder}` substitution (ONE style, resolved lazily by
the engine): `{thisfiledir}` / `{thisfilestem}` / `{thisfilename}` /
`{thisfilepath}` are relative to the file the pattern appears IN; `{topdir}`
is the project top; `{interfaces}` / `{usecases}` are the global dirs. Ask the
engine, never guess: `"$ITER_BIN" resolve --project "$ITER_PROJECT"
[--node <key>] "<pattern>"` prints the resolved value(s).

**Never write any `*.iter.md` skeleton from memory.**
`"$ITER_BIN" validate --file <path> --template` prints the current
authoritative template for the nodetype named by the filename (the file need
not exist yet; an existing interface file's `kind:` steers which skeleton you
get). Fetch it when you create or restructure a file — the template is the
single deterministic source, so format changes reach every agent at once.

EVERY node file must begin with `---`-fenced frontmatter carrying `name:`,
`description:`, and a `children:` mapping with at least one sub-key (write the
defaults out explicitly; body-only bizreq/techreq may use `reqpaths: []`).

- Code node (`*.code.iter.md`):

      ---
      name: "Human-Readable Name"
      level: component        # context | container | component
      description: "one line on what this code node is"
      owner: bespoke          # bespoke | oss | 3rdparty
      teststate: inherit      # omit | include | block | inherit
      children:
        codedirs:   ["{thisfiledir}/"]          # the actual source code
        codenodes:  []                          # child *.code.iter.md files (paths/globs)
        inputs:     []                          # interface FILES consumed
        outputs:    []                          # interface FILES produced
        bizreqs:    ["{thisfiledir}/*.bizreq.iter.md"]
        techreqs:   ["{thisfiledir}/*.techreq.iter.md"]
        testgroups: ["{thisfiledir}/test/*.testgroup.iter.md"]
      ---

  **The code node file defines the C4 object — every file belonging to it is
  linked here, never inferred from directory positions.** `context`-level nodes
  attach to the project head automatically; containers/components attach where
  a parent's `codenodes` lists them (unlinked ones orphan). A testgroups link
  matching nothing = this node is deliberately untested; if tests should
  exist, create them where the link will find them.

- Interface node (`*.interface.iter.md`) — the LOGICAL data contract between
  code nodes; a GLOBAL object living under `{interfaces}`. FIXED FORMAT —
  these sections and ONLY these, enforced by `iter validate` (get the current
  skeleton with `"$ITER_BIN" validate --file <path> --template`; never write
  one from memory):

  - frontmatter: `name:` (the id); `kind:` — the interaction shape,
    `request-reply | event | stream | dataset`, never a transport or a syntax;
    `description:` (quoted prose); `children:` with the per-node defaults
    (`{thisfiledir}/{thisfilestem}/*.bizreq|techreq|testgroup.iter.md`)
  - one `# <id> — contract` H1, then a named summary under 300 characters
  - the kind's H2 sections: request-reply → `## Request`, `## Reply, success
    shape`, `## Reply, failure shape`; event → `## Event`; stream →
    `## Stream item`, `## Stream end`; dataset → `## Record`
  - closing every file, in order: `## Worked examples` (normative pairs in one
    strict-JSON fence) and `## Invariants` (few bullets — only what examples
    cannot show)
  - optionally, and ONLY as the final section after `## Invariants`:
    `## Exceptions` — a declared deviation from the internal transport law that
    service-to-service calls ride the mesh with mutual TLS and speak gRPC. State
    what deviates (e.g. a component that must speak an infrastructure wire
    protocol such as Redis's or Kafka's), why gRPC is impractical there, and
    what still holds (mesh transit, mTLS). Most contracts have no such section,
    and that is the normal case: no section, no exception

  Message shapes are pseudo-JSON with constraints as inline comments. JSON is
  the NOTATION, not a wire format — conventions for what JSON cannot say:
  binary as hex/base64 strings, 64-bit ints and money as strings or integer
  cents, timestamps as ISO-8601 strings, enums as closed vocabularies named in
  comments. A function call is logically request → reply, so a library surface
  is written as kwargs-object → return-object messages. Tag a fence (```json)
  only when the block strictly parses; pseudo-examples stay untagged. Models:
  `sampleV1/interfaces/ledger-command/` (request-reply) and
  `sampleV1/interfaces/entry-recorded/` (event — note the different fixed
  sections `kind:` demands; `iter validate --file <f> --template` prints the
  right skeleton for the kind).

  **The two-clause test — the file is right when:** (1) a stranger could
  implement EITHER side from this file alone, and (2) nothing in it would
  change if a component were rebuilt in another language or deployed
  differently. An interface is TRANSPORT-NEUTRAL: the same messages may ride
  an HTTP body, a gRPC message, a queue record, argv/stdout, or an in-process
  struct, chained in any order, without renaming a field or changing a rule.
  Carrier bindings — routes, ports, topics, flags, exit codes, build and
  deploy facts — fail clause 2: record them on the code node file of the object
  that serves that binding, never here.

  **WHAT, never WHO or HOW.** An interface is used by many code nodes; the file
  must not name providers, consumers, or callers, nor describe how or where
  anyone uses it — the structure graph already carries that through the code
  nodes' `children.inputs`/`children.outputs` links. Sentences like "consumed
  by X" go stale the moment a second consumer appears, and `iter validate`
  flags them.

  **Interfaces are shared contracts — reuse before creating.** Before adding a
  new interface, check the existing ids (every `*.interface.iter.md` in the tree
  — the scan aggregates them globally) for a contract that already fits;
  extend or reference it instead of creating a near-duplicate. Create a new id
  only when no existing contract covers the need. NEW interface files are named
  `<id>.interface.iter.md` and land in `$ITER_INTERFACE_DIR` (the
  `globalinterfacedir` / `{interfaces}` setting, default `{topdir}/interfaces/`)
  — the scanner finds them anywhere, but new ones belong there.

  **Copy the quotes** on the prose fields (`name`, `description`, `endpoint`). Prose
  routinely contains a colon-plus-space, and while the engine parses that fine
  unquoted, strict-YAML tools reading the same file refuse the whole block. Bare
  single-token values (`level:`, `kind:`, `owner:`, `teststate:`) stay unquoted.

  Every code node's BODY must contain a `# Long Description` section: a
  plain-language description of the object for a non-technical reader — describe,
  don't state; no jargon; define every acronym on first use ("three letter
  acronym (TLA)"); link related project parts by their node file path. Write it
  for real when you create or substantially change a node — never leave `TBD`.

- Use-case nodes (`*.usecase.iter.md`) are GLOBAL objects under `{usecases}`.
  Their `children.codenodes` is the REQUIRED link list (the *.code.iter.md
  files the journey needs — an empty list is valid and marks work to come);
  edit it with the engine-owned path: `"$ITER_BIN" usecase --project
  "$ITER_PROJECT" --file <f> --add <code file> [--remove <entry>] [--list]`.

- **Project-WIDE context** is `$ITER_MAINFILE` (the main.iter.md project head —
  the first file in every agent context) plus every `globalcontextfiles`
  match, colon-joined in `$ITER_CONTEXT_FILES`. The engine lists exactly those
  in your spin-up context automatically. Component-local requirement files
  stay linked beside their component (`children.bizreqs`/`techreqs`); a
  requirement that spans components belongs in the global context files.

If you encounter a node file with missing or malformed frontmatter anywhere in the
files you read for your work, correct it as part of your change and note the fix in
your output — ongoing verification is every agent's job. The deterministic check
is built in — after touching any iter file, run:

    "$ITER_BIN" validate --project "$ITER_PROJECT" --file <the file> --fix

`--fix` applies the safe corrections (fence normalization, quoting prose values
that contain ": "); everything else is reported for you to correct by hand.
Exit 0 = clean (info-level notes are fine); exit 1 = warn/error findings remain —
fix them before finishing.
