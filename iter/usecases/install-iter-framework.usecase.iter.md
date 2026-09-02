---
name: "Install iter framework"
description: "Getting started: install iterapp, scaffold a project, and run the first loop"
children:
  codenodes: []
  testgroups: ["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"]
---

# Install the iter framework

The getting-started use-case for iterapp itself: from an empty directory to a
running loop with agents picking up work. (This is a seeded starter — edit it,
point its codenodes at your real nodes, or delete it.)

## Steps

1. **Get the binary.** Build from source (`cargo build --release`) or copy a
   built `iter` onto the box. Deploy with a fresh inode (`rm` + `cp`, or
   `cp` + `mv`) — never overwrite a live executable in place.
2. **Scaffold.** `iter init .` (or just `iter start` — missing pieces heal on
   boot) creates `.iter/` (agent personas in `agents/`, pre/post steps in
   `prepostwork/`, engine config in `.engine/config.json`) plus the two
   structureV2 head files: `.iter/config.iter.json` (server settings) and a
   stub `main.iter.md` at the top directory (the project definition, first
   into every agent context).
3. **Describe the project.** Fill in main.iter.md, then add `*.code.iter.md`
   nodes as you go — a `level: context` node attaches to the project head,
   and its `children.codenodes` links attach everything below it.
4. **Tune.** Review `.iter/.engine/config.json` (models, budgets, agent caps)
   and Project Settings in the webapp (scan dirs, context files).
5. **Run.** `iter start` launches the engine plus this webapp and prints the
   URL. `iter stop --wait` drains cleanly.
6. **Feed it.** Create the first work item — from a Projects node, the New
   WorkItem form, or `"$ITER_BIN" add ...` — and watch the loop take it from
   `queued` to `complete`.
