# Capability: write or restructure an `*.iter.md` node file

Read this before you create, move, or substantially change any `*.iter.md` file. The
naming law (the dot rule, the seven nodetypes, children links, placeholders) is in
the shared instructions and always applies; this file is the AUTHORING detail — what
the frontmatter and body of each kind must contain. Interface files have their own,
longer rules: see `_interface_contracts.md`.

**Never write any `*.iter.md` skeleton from memory.**

    "$ITER_BIN" validate --file <path> --template

prints the current authoritative template for the nodetype named by the filename
(the file need not exist yet). The template is the single deterministic source, so
format changes reach every agent at once. Start from it every time.

## Code node (`*.code.iter.md`)

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

**The code node file defines the C4 object — every file belonging to it is linked
here, never inferred from directory positions.** `context`-level nodes attach to the
project head automatically; containers and components attach where a parent's
`codenodes` lists them, and unlinked ones orphan. A `testgroups` link matching
nothing = this node is deliberately untested; if tests should exist, create them
where the link will find them.

One code node per component directory, usually alongside that component's
requirement files — a `<component>.code.iter.md` works well.

After writing nodes, check what stranded and link it:

    "$ITER_BIN" orphans --project "$ITER_PROJECT"

## Requirement files (`*.bizreq.iter.md` / `*.techreq.iter.md`)

Same frontmatter law — `name`, `description`, and a `children:` mapping. When the
body itself holds the requirements rather than pointing at other files, write
`children.reqpaths: []` explicitly.

Component-scoped requirements go beside their component, linked by that node's
`children.bizreqs` / `children.techreqs` globs. PROJECT-WIDE requirements go where a
`globalcontextfiles` pattern in `$ITER_MAINFILE` will load them — that is the one
spot that configures always-loaded context for every agent.

## The `# Long Description` section (required in every code node body)

Every code node's BODY must contain a `# Long Description` section: a plain-language
description of the object for a non-technical reader — describe, don't state; no
jargon; define every acronym on first use ("three letter acronym (TLA)"); link
related project parts by their node file path. Write it for real when you create or
substantially change a node — never leave `TBD`.

## Copy the quotes

**Copy the quotes** on the prose fields (`name`, `description`, `endpoint`). Prose
routinely contains a colon-plus-space, and while the engine parses that fine
unquoted, strict-YAML tools reading the same file refuse the whole block. Bare
single-token values (`level:`, `kind:`, `owner:`, `teststate:`) stay unquoted.

## Check your work

After touching any iter file:

    "$ITER_BIN" validate --project "$ITER_PROJECT" --file <the file> --fix

`--fix` applies the safe corrections (fence normalization, quoting prose values that
contain ": "); everything else is reported for you to correct by hand. Exit 0 =
clean (info-level notes are fine); exit 1 = warn/error findings remain — fix them
before finishing.
