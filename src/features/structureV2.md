# Overview
After looking at `iter markers` and *.iter.md files, we need a major overhaul to make our *.iter.md files more consistent.
Today there is an organically grown mish-mash of file requirements, formats, and organizations. For example:
- marker.iter.md is bound to its parent directory, but testgroup.iter.md is not.
- interface.iter.md requires frontmatter, but testgroup and bizreq/techreq do not.

This update drives consistency and clarity by setting explicit rules:
- ONE placeholder style: `{key}` substitution, resolved lazily by the engine. (The older bash/envvar-style placeholder idea is dead; remove any support for it.)
- ONE head: previously there was a "project" (a C4 Object head) and an unnamed "everything under this iter instance" (the PWD), frequently the same by happenstance. These merge: **1 server = 1 project**. If you want 2 projects, run 2 servers.
- ONE linking mechanism: nodes join the DAG only through explicit `children` links (which may be globs). Directory nesting alone never links anything.
- ONE naming rule: every node file is `*.nodetype.iter.md` (see File Naming below).

We're renaming marker.iter.md to code.iter.md, for 2 reasons: (a) everything now becomes essentially a marker file, and (b) the previous marker file really only pointed to code, so code is a better prefix.

Any code.iter.md file can own code; it's expected components will own most of it. Containers may own some larger bundles of components, such as other OSS projects or larger libraries that we don't typically want to own/modify. While not enforced, we should have logic (maybe in the ingest agent) that double-checks that having containers own code directly is the correct choice.

While the WebUI iterloop component shouldn't change at all, the WebUI iterapp section will have moderate changes, adapting to the adjusted data structure and behavior.

## What's not changing?
The goal and core operations of the engine, and *.iter.md files as embedded definition files inside the repo, with their major components:
- code.iter.md — still defines the overall object / node (previously marker.iter.md, same basic purpose)
- testgroup.iter.md — still defines a series of related tests
- bizreq.iter.md / techreq.iter.md — still define the business and technical requirements
- interface.iter.md — still defines the interface between C4 objects
- usecase.iter.md — still defines high-level, testable user-workflows that drive TDD

Also not changing: technical organization
- iterloop (engine) and iterapp (webUI)
- core CLI and API interfaces / functions (except where consistency is needed)
- deployment of iter as a single binary, which includes templates to allow a full environment to be quickly stood up

The iterloop WorkItem engine/structures are *mostly* not changing, because workitems are not always 1:1 with the iterapp node that created them — a workitem may span multiple project nodes (code node, interface, usecase, etc.) and update many things at once, or reconcile one thing to another.
**One exception:** the workitem `codepath` (singular string) becomes `codepaths` (list of strings). A code node may declare multiple `codedirs`, and all of them feed the workitem's context. This should be uncommon, only affects the context supplied to the AI Agent, and is a mechanical string → list-of-strings change.


# File Naming
Node files match the explicit pattern `*.nodetype.iter.md`. Suffix-only matching (`*code.iter.md`) is dead — it collides with the "prefix anything" rule (`barcode.iter.md` would match `*code.iter.md`, `domain.iter.md` would match `*main.iter.md`).

Rules:
- The nodetype segment must be preceded by a dot, **unless** the file has no prefix at all.
- The nodetype segment is lowercase; matching is case-sensitive on the nodetype.
- Valid: `my_thing.code.iter.md`, `code.iter.md`, `.code.iter.md`, `my,super.duper.thing.code.iter.md`
- Invalid: `my_thing_code.iter.md`, `my-code.iter.md`, `my_thing.Code.iter.md`

Nodetypes: `main`, `code`, `interface`, `usecase`, `bizreq`, `techreq`, `testgroup`.

## Globs are recursive
All glob patterns in iter behave as recursive globs ("rglob"): `folder/**/*.iter.md` matches with and without intermediate subfolders — both `folder/subdir/my.iter.md` and `folder/my.iter.md` match.


# Placeholders
Rather than fight with environment variables, iter uses dynamic string placeholders for paths in the DAG. The format is `{key}`, replaced by a value or the output of an engine function.

**Resolution is lazy, at runtime.** The source string in the .iter.md file never changes; the engine resolves it each time it's used and never persists the resolved form. Moving files therefore never stales stored paths — the next resolution just picks up the new reality.

Path hygiene rules for the resolver:
- Directories always close with a trailing `/`. The resolver corrects double-slashes (`*//*` → `*/*`).
- To be forgiving, the resolver appends a missing trailing `/` to confirmed directories (only).
- If a placeholder resolves to a **list** (e.g. `{codedirs}`), the containing pattern expands cartesian-style: one pattern per list entry.
- If a user enters a single string where a list is expected, accept it and coerce to a 1-item list during validation.

## File-relative placeholders
Resolved relative to the .iter.md file in which the placeholder appears — always the marker file itself, never the code it points at. This consistency is what keeps path resolution predictable.
- `{thisfilepath}` = full filepath of the file in which this placeholder appears
- `{thisfilename}` = just the filename (no path)
- `{thisfilestem}` = `{thisfilename}` minus the extension (`.iter.md` counts as the extension and is removed, along with the nodetype segment's dot-suffix)
- `{thisfiledir}` = `{thisfilepath}` minus the filename; the owning parent folder

## Engine placeholders
- `{iter}` = full path to the local iter engine executable, so CLI calls become `{iter} --key value`
- `{iterdir}` = directory containing the iter engine executable

## Config-derived placeholders
Every key in `.iter/config.iter.json` and in `main.iter.md` frontmatter becomes an available `{placeholder}` for downstream nodes (see Project Server below).

Since `{thisfiledir}` and friends are meaningless without knowing *which* file, engine-side resolution APIs take a node identity, e.g. `iter --resolve --node <key> "{thisfiledir}/x"` returning the fully validated path. (Exact CLI shape TBD during build.)


# Project Server
New top-most entity in the hierarchy, merging the running iter server and the "project" definition. Two files drive this level (both configurable via the iterapp webUI):

## `.iter/config.iter.json`
Holds all global SERVER settings that will not change frequently — one of the few files required to exist in `.iter/` (a 1-time set-and-forget). This file drives the iter engine but is NOT included in any agent context. Keys (each becomes a `{placeholder}`):
- `mainfile`: path to the `main.iter.md` project definition file, which can live anywhere
- `iterglob`: the glob pattern identifying any iter node file; defaults to `**/*.iter.md` (replaces `marker_glob`)
- `topdir`: a singular top-level directory. The engine never uses `{topdir}` directly — it exists purely as a convenience placeholder that other settings' defaults hang off of, giving configuration a central starting point so it's easy to predict how the tree unfolds. (A user could inline every path and never use it.) Defaults to `{thisfiledir}/../`, i.e. the parent of the `.iter/` folder; prompted for change during first setup. Example: pdy-dev sets `topdir` = `{thisfiledir}/../../` since config.iter.json resides in `pdy-dev/devops/.iter/` and the topdir should be `pdy-dev/`.

## `main.iter.md`
Project settings and descriptions that evolve with the project, and the **first file included in ANY agent context**. Typically the first document created for a new project (the guiding high-level vision), updated and read constantly — recommended to reside in the MAIN PROJECT ROOT for ease of finding. Naming follows the standard rule: `main.iter.md` or `AnyPrefix.main.iter.md`.
- frontmatter (each key becomes a `{placeholder}`):
  - `projectname`: name of the project
  - `projectdescription`: short description, usually for UI purposes
  - `globalscandirs`: list of 1-to-N directories the engine scans (using `{iterglob}`) for node files to ingest. E.g. `{topdir}/` or `[{topdir}/core/, {topdir}/infra/, {topdir}/sdk/]`
  - `globalinterfacedir` (alias `{interfaces}`): the global folder containing the `*.interface.iter.md` files (in subdirs). Default: `{topdir}/interfaces/`
  - `globalusecasedir` (alias `{usecases}`): the global folder containing the `*.usecase.iter.md` files (in subdirs). Default: `{topdir}/usecases/`
  - `globalcontextfiles`: list of 0-to-N file globs always loaded into new agent context — including what were previously the separate `global_bizreq_path` / `global_techreq_path` settings, which are absorbed here and die as standalone settings. Keeping EVERYTHING loaded into new agent context in ONE spot reduces context bloat and gives one-stop maintenance. E.g. `{topdir}/reqs/*.iter.md`, or `[{topdir}/reqs/*.bizreq.iter.md, {topdir}/reqs/*.techreq.iter.md, ~/allprojects/mycontextfiles.md]`
- body: long, high-level markdown description of the project — first content included in context.

**Settings audit:** many V1 settings collapse or become unneeded under this design (`projects.json`, `marker_glob`, `global_bizreq_path`, `global_techreq_path`, ...). During the build, review every existing setting: eliminate duplication and keep only what's actually needed. The iterapp UI drops the project switcher but keeps the "Running Servers" list in the left nav pane.


# Common .iter.md File Structure
Every node file — all nodetypes, no exceptions — requires frontmatter containing all three of:
- `name`: name of the node
- `description`: short description, used for UI and context-index look-ups, so make it relevant
- `children`: a mapping of typed link-lists (paths, folders, or globs) pointing to dependent files; valid sub-keys per nodetype below. The `children` key must be present with at least one sub-key on every node. Where a sub-key has a default, writing the default out explicitly is encouraged (templates and agents do this); for body-only bizreq/techreq nodes, an explicit `reqpaths: []` satisfies the rule.

In addition, every nodetype that has "Additional frontmatter keys" (below) must include those keys for its type.

The frontmatter is followed by a markdown body: free-form detail on the node — what it does, how it operates, expected output. This is used as context for AI Agents. Much of it will be generated and maintained by agents too; e.g. a user won't hand-enter all `codenodes` for a usecase, they'll ask an agent (via workitem) to traverse and populate them.

## Additional frontmatter keys, by nodetype
- code.iter.md
  - `level`: `context` | `container` | `component` (the only three valid values)
  - `teststate`: `omit` (do not test self or descendants) | `include` (force test of self and descendants, even if parent is omit) | `block` (never test self or descendants, regardless of parent; only a human lifts a block, by editing the file) | `inherit` (default; do whatever your parent did)
  - `owner`: `bespoke` | `oss` | `3rdparty`; default `bespoke`; who authored this unit of code, and whether it's practical to modify
- interface.iter.md
  - `teststate`: same values/semantics as above
  - `owner`: same values/semantics as above
- usecase.iter.md
  - `teststate`: same values/semantics as above

`teststate` is the rename of the V1 `test_loop` key; V1 value `blocked` becomes `block`. Migration rewrites both key and values; the "human must lift a block by hand" semantics carry over unchanged.

## `children` sub-keys, by nodetype
- code.iter.md
  - `codenodes`: optional; no default; any other `*.code.iter.md` files
  - `codedirs`: optional; default `[{thisfiledir}/]`; the actual source code, including all descendant content. Typically `{thisfiledir}` is the top of the code, but it's not required — code living elsewhere is referenced via e.g. `{topdir}/src/...`
  - `inputs`: optional; no default; interfaces consumed as inputs by this node's code, usually in `{globalinterfacedir}/**/*.interface.iter.md`
  - `outputs`: optional; no default; interfaces produced as outputs by this node's code, usually in `{globalinterfacedir}/**/*.interface.iter.md`
  - `bizreqs`: optional; default `[{thisfiledir}/*.bizreq.iter.md]`
  - `techreqs`: optional; default `[{thisfiledir}/*.techreq.iter.md]`
  - `testgroups`: optional; default `[{thisfiledir}/test/*.testgroup.iter.md]`
- bizreq.iter.md
  - `reqpaths`: optional; no default. Bizreq holds actual requirements in the MD body; optionally it may also point at existing external requirement docs via `reqpaths`, e.g. `[{thisfiledir}/requirements/my_older_requirements.md, {topdir}/my_global_requirements/reqs.md]`. Body content and reqpaths may coexist.
- techreq.iter.md
  - `reqpaths`: optional; same behavior as bizreq's `reqpaths`.
- interface.iter.md (lives in the shared `{globalinterfacedir}`, so defaults use a per-node subfolder to avoid siblings claiming each other's files)
  - `bizreqs`: optional; default `[{thisfiledir}/{thisfilestem}/*.bizreq.iter.md]`
  - `techreqs`: optional; default `[{thisfiledir}/{thisfilestem}/*.techreq.iter.md]`
  - `testgroups`: optional; default `[{thisfiledir}/{thisfilestem}/*.testgroup.iter.md]`
- usecase.iter.md (lives in the shared `{globalusecasedir}`; same per-node subfolder rule)
  - `codenodes`: **required key, may be empty**; `*.code.iter.md` files whose code is required to satisfy this usecase. An empty list is valid and spawns a TODO workitem for an agent to traverse and populate it.
  - `testgroups`: optional; default `[{thisfiledir}/{thisfilestem}/*.testgroup.iter.md]`
- testgroup.iter.md
  - `testpaths`: optional; default `[{thisfiledir}/*.sh]`

### Example: mykafkalib.code.iter.md
```yaml
---
name: My New Library
description: A library to support some stuff, and do amazing things.
level: container
owner: bespoke
teststate: inherit
children:
  codedirs:   ["{topdir}/src/my_kafka_plugin/"]
  codenodes:  ["{codedirs}/**/consumermods.code.iter.md", "{codedirs}/**/producermods.code.iter.md"]
  inputs:     ["{interfaces}/kafka/mylib.interface.iter.md", "{interfaces}/kafka/kafka_mylib.interface.iter.md"]
  outputs:    ["{interfaces}/kafka/kafka_consumer.interface.iter.md", "{interfaces}/kafka/kafka_mylib.interface.iter.md"]
  bizreqs:    ["{thisfiledir}/bizreq.iter.md"]
  techreqs:   ["{thisfiledir}/techreq.iter.md"]
  testgroups: ["{thisfiledir}/test/*.testgroup.iter.md"]
---
```

### Example: some_test.testgroup.iter.md
```yaml
---
name: Test Kafka Plugin Consumer
description: Test my new kafka consumer plugin, make sure it works.
children:
  testpaths: ["{thisfiledir}/*.sh"]
---
```

### Example: confirm_order.usecase.iter.md
```yaml
---
name: User gets confirmation after order
description: Authenticated user places an order in the system, and gets a confirmation (running thru kafka)
children:
  codenodes:  ["{topdir}/src/my_kafka_plugin/mykafkalib.code.iter.md", "{topdir}/src/auth/authentication.code.iter.md", "{topdir}/src/authz/authorization.code.iter.md"]
  testgroups: ["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"]
---
```


# DAG Rules
- Nodes join the DAG **only** through explicit `children` links. Links may be exact paths or globs ("fuzzy links"): a node declaring `testgroups: ["{thisfiledir}/test/*.testgroup.iter.md"]` picks up any new testgroup file dropped into `test/` automatically — but that's the declared link doing the work, never the directory nesting itself.
- Soft cap of 10 layers.
- No cycles. When ingest detects a cycle, the offending edge is demoted and the resulting unlinked subtree goes to the Orphanage — not a hard validation failure.
- Executable nodes (code, interface, usecase) may have multiple parents; that's what makes this a DAG.
- **Testgroups have exactly 1 parent.** Every test run has one starting node. That node's traversal may cascade to many nodes, and two test runs may cover overlapping code — that has always been true.
- `teststate: inherit` is evaluated **per traversal chain**, not per node: if usecase A's chain says include and container B's chain says omit, the shared code node is tested by A's run and skipped by B's run. Both are simultaneously correct.
- Forking: the application allows "forking" a node, which makes a new physical copy that can deviate over time (a copy, not a link).

## Ownership Tree
- ROOT: project-server
  - Code: `context` level by default belongs to root (but can be moved down the tree; containers/components can be moved up to belong to root)
    - nested code nodes, typically context → container → component
    - code owns bizreq/techreq, and links interface usage (inputs and outputs) but does not own interfaces
    - owns testgroups
  - Interface: globally stored
    - linked from code as an input or output interface (linked, not owned)
    - owns testgroups
  - UseCase: globally stored
    - links to code nodes (linked, not owned)
    - owns testgroups

The ownership tree holds the 3 "executable" node types: code, interface, usecase. Testgroups are always at the bottom.

## Orphanage
Files that match `{iterglob}` and the naming rules but are not linked into the DAG land in the Orphanage.
- Add the Orphanage as a 4th collapsible section in the iterapp UI, with an interface to quickly link an orphan under the node where it belongs.
- A daily scheduled workitem evaluates the orphanage and opens a TODO recommending where each orphan likely goes (leaving it in the orphanage is a viable recommendation). Do NOT open a new workitem if every current orphan is already covered by a still-TODO workitem.


# Testing
The way testing works does NOT change; each testgroup identifies its own list of 1-to-N tests, run in sequence. Tests are still:
- bash scripts returning 0 (success), 1 (failure), or >1 (test error)
- bundled into testgroups; the testgroup is the level at which tests are managed and run

Nodes that can be tested are the three executable node types: code, interface, usecase.

When a testgroup fails (returns 1):
1. create a workitem to resolve it
2. find the testgroup's singular parent node
3. take all `codedirs` from that parent and set them as the workitem's `codepaths` (the list-form replacement of the old singular `codepath`)


# Engine Additions
As linking shifts from directory structure to explicit marker references, the `iter` engine must digest, link, and answer structural questions directly — many paths that used to be inferable from folder layout now require lookups against the ingested DAG.

Example: a usecase surfaces several issues and opens several workitems at once. Each workitem needs codepaths that resolve differently per node being worked. The engine must answer "given node A, the codepaths resolve to B" — hence node-aware resolution APIs like `iter --resolve --node <key> "{placeholder}/path"`. Exact shapes TBD during build.


# Migration & Testing Plan
Two separate tracks:
1. **Greenfield validation:** `sampleV2/` starts EMPTY. V2 code builds a fresh sample project into it from scratch, proving the V2 engine end-to-end.
2. **Migration validation:** the migration converts `sampleV1/` in place to V2 format, proving V1 → V2 conversion on a real V1 project.

Migration is a **one-time throwaway** — pdy-dev is the only real project on iter, so don't overthink it. No standalone binary needed; performing the migration directly (Claude-driven, with deterministic scripting where easy) is fine. The data must end up safe and representative of its meaning in V2; the tooling doesn't survive.

Migration's primary job is re-establishing links: V1's directory-implied parent/child relationships must become explicit `children` entries. Approach: write deterministic links (obvious 1:1 mappings) directly, queue the fuzzier link decisions as workitems for Ingest agents, then double-check everything. Also rewrites: `marker.iter.md` → `code.iter.md` filenames, filename dot-rule compliance, `test_loop` → `teststate` (`blocked` → `block`), `projects.json`/split-config → `.iter/config.iter.json` + `main.iter.md`.
