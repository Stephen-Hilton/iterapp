# iterapp / iterloop

Iterloop is an AI coding harness: a Rust engine that loops over a file-based queue of
work items and delegates each to a purpose-built headless Claude Code agent. Agents,
pre/post work steps, and source instructions are all markdown files — adding a file adds
a capability. Full specification: [`src/features/iterloop.md`](src/features/iterloop.md).

## Deploy = copy one file

`iter` is a single self-contained binary — the iterloop engine, the iterapp webapp,
and the `.iter/` template all ship inside it. Drop it into any project directory and
start it; missing `.iter/` folders and files are created on the spot (existing files
are never overwritten):

```bash
cargo build --release
cp target/release/iter ~/dev/myproject/
cd ~/dev/myproject && ./iter start
#   initialized 21 missing .iter file(s) in .
#     iterapp webapp:  http://localhost:9889/
#                      http://myproject.localhost:9889/
```

`iter start` runs the engine loop AND the webapp server, printing the URL to copy or
open. The port is deterministic (hashed from the project path into 9700–9899, so the
same project always gets the same port; `--port N` to pin one), and the hostname slug
comes from `url_slug` in `.iter/projects.json`, defaulting to the directory name.
Use `iter run` for the engine alone, headless.

The binary is per-platform (build on the OS/arch you deploy to), and agent
execution shells out to `claude` (plus `git`/`gh` for those prepostwork steps) —
those must be on PATH; everything else is in the one file.

## Quickstart

```bash
cargo build

# initialize a target project (embedded template; --from <dir> to use your own)
./target/debug/iter init ~/dev/myproject

# add a work item
./target/debug/iter add --project ~/dev/myproject \
  --type code --title "add auth middleware" \
  --mainwork "Implement ... acceptance criteria ..." --codepath "./api" --priority 6

# run the engine (Ctrl-C, or `iter stop` from another shell)
./target/debug/iter run --project ~/dev/myproject

# inspect
./target/debug/iter status --project ~/dev/myproject
./target/debug/iter stop   --project ~/dev/myproject --wait   # drain, then stop
```

`run --once` executes a single tick; `run --until-idle` exits when the queue drains —
both useful for scripting and demos.

## Try it on the bundled sample

`sample/` is a tiny toy project (a POSIX `greet.sh` with real tests) pre-seeded with
work items. Copy it somewhere and point the engine at it:

```bash
cp -R sample /tmp/demo
./target/debug/iter run --project /tmp/demo --until-idle
```

## Fake runner (no tokens)

Set `ITER_CLAUDE_BIN` to any executable that prints
`{"session_id":"...","result":"..."}` and the engine runs the full loop — locking,
lifecycle, handoff — without calling Claude. The integration tests in `tests/e2e.rs`
use this; see `setup_project` there for a reference stub.

## Layout

- `src/*.rs` — the engine (config, agents, workitems, locks, context, runner, scheduler, CLI)
- `src/.iter/` — the shipped template: agent definitions, prepostwork steps, source
  instructions, engine config
- `src/features/iterloop.md` — the specification and build plan
- `sample/` — mock target project used by tests and demos
- `tests/e2e.rs` — end-to-end tests against the real binary with the fake runner

## Notes

- Agents create handoff work items by running `iter add`; put the binary on `PATH`
  (or reference it absolutely in your agent files) so spawned agents can find it.
- The template agent files ship with `--dangerously-skip-permissions` for sandboxed
  demos. Remove it (and rely on your repo's own `.claude/` permission settings) before
  pointing iterloop at production code.
