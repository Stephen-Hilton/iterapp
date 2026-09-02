---
projectname: "iterapp"
projectdescription: "iterapp building itself — the Rust engine, webapp, and CLI in this repo, managed by its own queue"
globalscandirs: ["{topdir}/iter/"]
globalinterfacedir: "{topdir}/iter/interfaces/"
globalusecasedir: "{topdir}/iter/usecases/"
globalcontextfiles: []
children:
  codenodes: ["{thisfiledir}/engine.code.iter.md"]
---

# iterapp

iterapp (the `iter` binary) is a Rust harness that loops headless Claude Code
agents over a durable work-item queue: a person (or an agent) files work items,
the engine schedules them under concurrency/spend/usage guards, spawns an agent
per item with composed context, and tracks every state transition in SQLite.
One running server per project; the webapp is the queue console.

THIS project is iterapp pointed at itself: the queue in `iter/` tracks changes
to the engine in `{topdir}/src/`. It is both real usage (file actual engine
work here) and a harmless test bed for iterapp's own features.

Ground rules for agents working this project:

- The code is `{topdir}/src/` (single Rust crate; the webapp is
  `src/webapp/app.html`, feature docs are `src/features/*.md`). Integration
  tests live in `{topdir}/tests/`, Playwright UI tests in `{topdir}/e2e/`.
- Finish engine changes with `cargo build --release` — deploys copy the binary
  from `target/release/`.
- Run `cargo test` before declaring work done; UI-visible changes should also
  pass `cd e2e && npx playwright test`.
- Do NOT edit `{topdir}/iter/` (this project's own queue home) from a work
  item unless the item explicitly says so — that is the harness you are
  running inside.
- Terminology and conventions: see `src/features/structureV2.md` and the other
  feature docs before inventing anything.
