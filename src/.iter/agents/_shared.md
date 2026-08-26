# Shared instructions (all agents)

This file is appended to EVERY agent's context on every run — the store-once place
for rules that apply to all agents, every turn. Keep entries short and universal;
agent-specific guidance belongs in that agent's own file, and mechanics an agent
needs only occasionally belong in a capability file (see the index below). Files
here starting with `_` are helpers, never agent types.

## Capabilities — an index; READ the file when you need the capability

Mechanics used occasionally live in their own files instead of this one, so agents
that never use them never carry them. Each line below names a capability, says when
you need it, and gives the file to read. Use your Read tool (or `cat`) to read that
file AT THE MOMENT you need the capability, and follow what it says — never work
from memory of what it probably contains. The directory is
`$ITER_PROJECT/.iter/agents/_capability/` (`$ITER_PROJECT` is an absolute path the
engine exports into your environment). Anywhere a file names a capability as
`_capability/<file>.md` or just `<file>.md`, that is the file to read — expand it to
`$ITER_PROJECT/.iter/agents/_capability/<file>.md`.

- **`_create_new_workitem.md`** — creating a work item with `iter add`: the JSON
  shape, every option, how to write the item's `mainwork`, `depends_on` ordering,
  automation inheritance, `codepath` / `codepath_ignore`, and which `model` to put
  on the item. Read it before every add. Two rules bind you even before you read
  it: **never set `state`** (the engine derives it from the automation mode), and
  write `mainwork` in the same three-tier shape as your own output.
- **`_ask_the_human.md`** — `iter ask`: stopping to put a decision to a human when
  it is not yours to make (what the product should do, which trade-off the project
  lives with), plus the four-part format every question must take. Read it the
  moment you are tempted to guess a product decision.
- **`_critical_review.md`** — `iter critreview`: the synchronous critique
  subprocess. Read it whenever your mainwork asks for a critical review or a
  critique. The review is part of the work, and a nonzero exit means your item has
  already been flagged to fail — stop there. The cap is a live setting (Settings →
  `critreview_max_rounds`), currently
  at most {critreview_max_rounds} review round(s) per work item.
- **`_reject_invalid_work.md`** — `iter reject`: what to do when the problem is the
  WORK ITEM itself (out of scope, goal unclear, premise no longer true, conflicts
  with a `*bizreq.iter.md` invariant) rather than your ability to do it. Rejecting
  is not failing. Read it before you fail — or quietly complete — a bad item.
- **`_runtests.md`** — `iter runtests`: the deterministic runner and its three
  modes — neutral runs, `--broken` (claims the defect is still present) and
  `--fixed` (claims it is resolved). A false claim flags your item as failed. Read
  it before any run that carries a claim, and before filing a defect-shaped item.
- **`_testgroup_authoring.md`** — the format law for `*.testgroup.iter.md` files
  and the shell scripts they register: the `iterapp:testgroups` JSONL block, the
  `testlist` entry shape, the exit-code / `ITER_RESULT` script contract, the three
  test flavors, and the registration chain that makes the sweep see a test at all.
- **`_iter_file_authoring.md`** — creating or restructuring a code node or
  requirement `*.iter.md` file: the frontmatter blocks, the required
  `# Long Description`, the quoting rule, and the orphan check.
- **`_interface_contracts.md`** — writing an `*.interface.iter.md`: the fixed
  section format each `kind:` demands, transport neutrality, WHAT-never-WHO-or-HOW,
  and reuse before creating a new id.
- **`_teststate.md`** — `iter teststate`: the Test Loop gate
  (`inherit` | `omit` | `include` | `block`), the nearest-flag-wins rule, and why a
  `block` refusal must never be forced or worked around.
- **`_usecase_links.md`** — `iter usecase`: editing a use-case file's
  `children.codenodes` link list from any agent, and the rule that links reflect
  what was BUILT, not what was proposed.

## Communicate clearly — a human must get it FAST

Goal: a human developer skimming your output understands the status, the
feedback, and any issues in seconds. Structure EVERY output you write (work
item outputs, observations, reports) as exactly three tiers, in this order:

1. **High-level summary** — a few sentences, no jargon, generous context. A
   reader who knows nothing about this work item must come away knowing
   (a) WHERE in the large codebase this work targets, (b) WHAT changed, and
   (c) WHY.
2. **Details** — everything else worth a human's eyes, as hierarchical
   bullets (nest sub-bullets to show structure). Short but descriptive:
   each bullet ideally fills one line, two lines at most. Numbered lists
   when order matters, bullets when it doesn't.
3. **Agent-level details** — at the BOTTOM: everything only a machine reader
   needs (exact commands run, raw test output, ids, file-by-file minutiae).
   Humans will likely never read this section; keep its content out of the
   two tiers above.

Style rules for all tiers (and everything else you write — commit messages,
docs, the `mainwork` of items you create):

- **Use specific, common words.** No jargon, no invented terms, no implied
  meanings. Call files and things by their exact names (`testgroup.iter.md`,
  not "the manifest").
- **Describe — don't just state or name.** "the lock file was never deleted, so
  every later run waits forever" beats "stale lock issue".
- **Use an analogy when the concept is abstract.** One good comparison to an
  everyday thing speeds understanding more than a paragraph of precision.
- **Avoid large blocks of dense text** — they slow human readers down. Break
  them up or cut them.

## Scratch files go in `$ITER_TEMP`

Anything you write for your own use within one work item's lifetime — a work item
draft you feed to `iter add --file`, a question file, review material, a one-off
helper script — goes in `$ITER_TEMP/`, the absolute scratch directory the engine
exports into your environment. **Never write a relative `.iter/temp/...` path**: it
resolves against your working directory and mints a stray temp tree wherever you
happen to be running. Files there are swept after `temp_file_ttl_days` (Settings),
so nothing that someone will want later belongs there.

## Lock scope and codepath_ignore

Your work item's `codepath` is your lock scope: the directory tree you own for
this run. If the item carries `codepath_ignore` patterns (gitignore-style,
relative to the codepath), those subtrees are **carved out of your scope — do not
create, edit, or delete anything under them.** Another work item may own them and
be running there right now; the engine's lock lets you both through on exactly
that promise. Reading is fine anywhere. When you create work items, use the same
mechanism to parallelize them — `_create_new_workitem.md` has the pattern.

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
single deterministic source, so format changes reach every agent at once. The
authoring rules for each nodetype are in `_iter_file_authoring.md` and
`_interface_contracts.md`.

EVERY node file must begin with `---`-fenced frontmatter carrying `name:`,
`description:`, and a `children:` mapping with at least one sub-key (write the
defaults out explicitly; body-only bizreq/techreq may use `reqpaths: []`).

**Project-WIDE context** is `$ITER_MAINFILE` (the main.iter.md project head — the
first file in every agent context) plus every `globalcontextfiles` match,
colon-joined in `$ITER_CONTEXT_FILES`. The engine lists exactly those in your
spin-up context automatically. Component-local requirement files stay linked beside
their component (`children.bizreqs` / `techreqs`); a requirement that spans
components belongs in the global context files.

If you encounter a node file with missing or malformed frontmatter anywhere in the
files you read for your work, correct it as part of your change and note the fix in
your output — ongoing verification is every agent's job. The deterministic check
is built in — after touching any iter file, run:

    "$ITER_BIN" validate --project "$ITER_PROJECT" --file <the file> --fix

`--fix` applies the safe corrections (fence normalization, quoting prose values
that contain ": "); everything else is reported for you to correct by hand.
Exit 0 = clean (info-level notes are fine); exit 1 = warn/error findings remain —
fix them before finishing.
