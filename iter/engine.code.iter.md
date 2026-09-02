---
name: "iter engine"
level: context
description: "The whole iter binary: engine loop, scheduler, storage, webapp server, and CLI — all of {topdir}/src/."
owner: bespoke
teststate: inherit
children:
  codedirs:   ["{topdir}/src/"]
  codenodes:  []
  inputs:     []
  outputs:    []
  bizreqs:    []
  techreqs:   []
  testgroups: []
---

# Long Description

The boundary is the single Rust crate in `{topdir}/src/`. Outside it: a person
at a terminal (CLI + webapp) and the Claude Code CLI it spawns. Inside it, the
main regions of `src/`:

- `main.rs` — CLI entry (start, init, add, status, export, testsweep, …)
- `scheduler.rs` + `workitems.rs` + `db.rs` — the engine loop, the queue, and
  its SQLite storage (five durable tables)
- `server.rs` + `webapp/app.html` — the per-project web server and console
- `agents.rs`, `context.rs`, `runner.rs` — agent definitions and the composed
  context an agent session starts with
- `markers.rs`, `project.rs`, `validate.rs` — structureV2: node discovery (the
  dot rule), the two head files, DAG validation
- `itersched.rs`, `limits.rs`, `spend.rs`, `testsweep.rs` — schedules, account
  usage throttling, the spend ledger, the deterministic test loop

Cross-cutting rules: durable state is SQLite only (`db.rs`); settings are the
engine config (`.iter/.engine/config.json`) plus the two head files; all engine
time math is UTC (user_timezone is display-only). Integration tests in
`{topdir}/tests/e2e.rs` drive a real engine against a fake `claude`; the
Playwright suite in `{topdir}/e2e/` drives the real webapp.

This node deliberately spans the whole crate for now. If work starts colliding
on codepath locks, split child code nodes out per region (scheduler, server,
webapp) with their own `codedirs`.
