# Iterloop — Feature Specification (v1)

Iterloop is an AI coding harness: a looping engine that reads a queue of work items,
selects the best next item(s), and delegates each to a purpose-built AI agent (a headless
Claude Code session). Agents are defined as markdown files; the queue, locks, and
configuration are plain files on disk. The whole system is file-first by design so it can
be inspected, versioned, and extended by adding markdown — not by changing engine code.

**v1 scope (this document):** get the data model, the file interface, and the engine loop
working correctly, with enough terminal output to verify behavior. Scheduling stays
deliberately simple (priority/age/source ordering, nothing smarter), but work-item
*handoff* is core from day 1: every agent can queue new work items, and they will
regularly do so. More complex interactions build on this framework later.

## Design decisions (locked 2026-08-10)

| Decision | Choice |
|---|---|
| Engine language | **Rust from day 1** — matches shipping architecture; no throwaway rewrite |
| Agent runner | **Headless `claude -p`** with `--resume <session-id>` for sequential prompts into one session |
| Queue location | **`.iter/.engine/workitems.jsonl`** — engine-owned data lives in the engine-owned folder |
| Concurrency | **Concurrent from day 1** — locking is core to the product, so exercise it immediately |
| Agent handoff | **All agents can create new work items, and regularly do** — work items are the standard handoff between agents; v1 write path is `iter add` (stateless MCP is the long-term candidate) |
| Retry semantics | **Full re-run** — a failed item retries from scratch: fresh session, prework included |
| Logging | **One stream**, tagged `[type#n]` |
| Type validation | **Warn at add, enforce at pick** |

## Goals and non-goals

### Goals (v1)
- Engine loop: load agents → find eligible work → lock → spawn agent → run prework /
  mainwork / postwork → record output → close item. Repeat.
- Multiple agents running concurrently against different codepaths, with correct locking
  so two agents never edit overlapping file sets at the same time.
- Everything user-extensible is a markdown file: agents, prework/postwork steps, source
  instructions. Adding a file adds a capability; no engine change needed.
- Work items as the standard handoff between agents: any agent can create new work items
  mid-run (via `iter add`), and the loop picks them up on a later tick.
- Terminal output sufficient to watch and verify the loop end to end.
- Runs locally on a laptop or server as a single binary.

### Non-goals (v1)
- No scheduling intelligence beyond the simple priority/age/source ordering below (no
  dependency graphs, no approval gates — the `todo` state is reserved for those).
- No web UI / web server (planned; see [Future](#future-v2) — design leaves room for it).
- No `.sh` deterministic prepostwork steps (markdown/AI steps only in v1; see below).
- No multi-machine or cloud deployment (but avoid decisions that would preclude a
  lightweight Linux container later).

## Architecture at a glance

```
┌─────────────────────────── iterloop engine (Rust binary) ───────────────────────────┐
│                                                                                     │
│  config + templates            scheduler (tick loop)          agent runner          │
│  .iter/agents/*.md      ──►    capacity per agent type  ──►   claude -p (headless)  │
│  .iter/prepostwork/*.md        find work / prioritize         --resume per prompt   │
│  .iter/source/*.md             record lock + codepath lock    timeout + capture     │
│  .iter/.engine/config.json                                                          │
│                                                                                     │
│  queue: .iter/.engine/workitems.jsonl  ──►  workitems_closed.jsonl (append-only)    │
└─────────────────────────────────────────────────────────────────────────────────────┘
                                        │ spawns, cwd = workitem.codepath
                                        ▼
                        target project (e.g. sample/)
                        code + tests + testgroup.iter.md + .iter.lock (while working)
```

Two distinct `.iter/` trees exist:

1. **`src/.iter/`** — the template that ships with iterapp. This is the canonical
   file/folder structure and default content, copied into a target project on init.
2. **`<target-project>/.iter/`** — a live instance in the project being worked on
   (see `sample/.iter/` for the mock target project used in development and testing).

### Extensibility rule
Any folder directly under `.iter/` is **extensible**: drop in a new markdown file and the
engine (and eventually the UI) discovers it by interrogating the folder. Any folder under
`.iter/` whose name starts with a period (e.g. `.iter/.engine/`) is **engine-owned**: its
contents are hardcoded into the engine's behavior and must not be added to or renamed by
users.

### Future file types
In later versions, extensible folders may hold multiple extensions per name — e.g.
`prepostwork/git-pull.sh` alongside `prepostwork/deploy.md`. The intent: the engine calls
deterministic bash (`.sh`) when appropriate, and makes AI agent calls (`.md`) when
appropriate. When both `deploy.md` and `deploy.sh` exist for one name, the AI agent runs
the nondeterministic instructions first, then uses that output to invoke the shell script,
which runs deterministically. **v1 supports `.md` only**, but name resolution is written
extension-aware so `.sh` slots in later without breaking anything.

## Directory layout

```
<project>/
  .iter/
    agents/                     # extensible: one file per agent type
      plan.md  code.md  test.md  testwriter.md  refactor.md  ingest.md
    prepostwork/                # extensible: shared pool of pre/post work steps
      git-pull.md  git-commit.md  git-push.md  git-pr.md  deploy.md
      iterloop-stop.md  iterloop-wait-for-stop.md
    source/                     # extensible: instructions keyed by workitem.source
      user.md  agent.md  error.md
    .engine/                    # engine-owned: do not extend or rename
      config.json               # engine + global settings
      workitems.jsonl           # open work items (the queue)
      workitems_closed.jsonl    # closed work items (archival, 100% append-only)
      codepath_lock.md          # engine procedure/template: acquire codepath lock
      codepath_unlock.md        # engine procedure/template: release codepath lock
```

## File specifications

### 1. Agent definitions — `.iter/agents/*.md`

One file per agent type. The **basename is the agent type** (`code.md` → type `code`),
which is also the legal set of values for `workitem.type`. The interface lists agents by
interrogating this folder; the engine reads the same file for configuration and prompt.

Structure: YAML frontmatter (config, machine-read) + markdown body (the agent's prompt
definition: role, focus, behavior, constraints, output expectations).

```markdown
---
description: "This is the coding agent"     # one-liner for UI/terminal listings
visible: true                # false hides from UI/listings; engine can still use it
max_agent_count: 3           # concurrency cap for this agent type
max_work_timeout_sec: 3600   # hard kill for a single prompt turn
max_connection_timeout_sec: 30   # spawn/handshake timeout
model: opus                  # model passed to claude -p
model_flags: "--dangerously-skip-permissions"   # extra CLI flags, passed through verbatim
llm_run_mode: headless       # v1: headless only (terminal/tmux reserved for later)
sleep_interval_sec: 30       # idle wait before this agent type re-polls for work
---

# Agent Definition
You are the {type} agent...   (role, focus, behavior, guardrails, output format)
```

Unknown frontmatter keys are ignored (forward compatibility). Missing keys fall back to
engine defaults in `config.json`.

### 2. Pre/post work steps — `.iter/prepostwork/*.md`

A single shared pool of steps usable as either prework or postwork. Each file is a
self-contained prompt for one step (e.g. `git-commit.md` describes exactly how to stage,
what the commit-message convention is, and what to report back). The workitem references
steps **by basename** (`"git-pull"`); the engine maps name → file content.

Resolution rule for each entry in `workitem.prework` / `workitem.postwork` — entries are
the **filename minus the extension** (`git-push.md` is referenced as `"git-push"`):
1. If `.iter/prepostwork/<entry>.md` exists → submit that file's content as the prompt.
   (Matching is extension-aware so `<entry>.sh` can join the lookup later.)
2. Otherwise → treat the entry itself as a literal inline prompt.

This keeps common steps reusable while allowing one-off instructions inline, and it maps
directly onto the planned UI: prework added from a **selector** (the filenames minus
extensions, discovered from the folder) *or* typed in as a free-form prompt — same field,
same resolution rule.

Engine-control steps: `iterloop-stop.md` and `iterloop-wait-for-stop.md` instruct the
agent to write a control signal (see [Engine control](#engine-control)) rather than touch
project code — this is how a work item can pause/stop the engine itself.

### 3. Source instructions — `.iter/source/*.md`

Keyed by `workitem.source`, these tell the agent how to treat work from that origin
(e.g. user-sourced work is trusted as stated; error-sourced work should reproduce first
and be skeptical of the description).

| `workitem.source` | file |
|---|---|
| `user` | `source/user.md` |
| `agent: {type}` | `source/agent.md` (engine substitutes `{type}` inside the content) |
| `error` | `source/error.md` |

> Rename note: the stub is currently named `source/agent ({type}).md`. It becomes
> `source/agent.md`; `{type}` is a template variable available in the file body, not part
> of the filename. (Spaces/parens in filenames buy nothing and complicate glob/CLI use.)

### 4. Engine files — `.iter/.engine/`

**`config.json`** — engine settings plus global defaults:

```json
{
  "engine": {
    "tick_interval_sec": 5,
    "agent_stagger_ms": 100,
    "queue_lock_retry_ms": 50,
    "queue_lock_break_sec": 60,
    "codepath_lock_timeout_sec": 3600,
    "codepath_conflict_backoff_sec": 15,
    "max_total_agents": 8,
    "max_open_workitems": 200,
    "retry_backoff_sec": 300,
    "max_attempts": 3
  },
  "globalsettings": {
    "testwriter_min_tests_per_group": 20,
    "testwriter_max_tests_per_group": 100,
    "test_dir": "test",
    "log_default_path": "./logs/{YYYYMMDD-hh}.log",
    "log_level": "info",
    "log_max_size_mb": 10,
    "log_max_files": 50
  }
}
```

**`workitems.jsonl`** — the queue: all work items **not** complete. JSONL (one JSON
object per line, no containing array) so new items can be appended quickly and cheaply.

**`workitems_closed.jsonl`** — archival, 100% append-only. Items move here on `complete`
(and on terminal `failed`, i.e. attempts exhausted).

**`codepath_lock.md` / `codepath_unlock.md`** — the engine's own procedure and content
template for acquiring/releasing `.iter.lock` files (format, timeout rules, stale-lock
handling). Engine-owned: the lock protocol is hardcoded behavior; these files document it
and provide the template the engine renders into `.iter.lock`.

### 5. Context files — `workitem.context`

Any entity adding a work item may attach any number of file paths, with or without
wildcards, drawn from anywhere on the machine: the codepath itself, a project-central
location (common interfaces, project-wide requirements), or a computer-central location
(centralized requirement management).

```
"context": [
  "{codepath}/*.md",
  "../some/other/dir/*.interfaces.md",
  "~/dev/global/rules/*req*.md"
]
```

The engine resolves and validates these **deterministically and quickly** (glob expansion,
existence check, `{codepath}` / `{reqs}` / `~` substitution) and hands the resulting concrete
file list into the built prompt. The agent then reads the files itself as part of spin-up —
the engine does not inline file contents into the prompt.

**Project-wide requirements (first-class, added 2026-08-15):** the global
`bizreq.iter.md` / `techreq.iter.md` live in the project reqs directory — default
`.iter/reqs/` (scaffolded by the template), relocatable per project via a `reqs:`
frontmatter key on the `level: project` marker at the code root (relative values
resolve against the code root, `~` ok). The engine auto-lists the directory's markdown
files in every work item's spin-up prompt (deduped against explicit context), exports
the resolved path to agent sessions as `ITER_REQS`, and substitutes `{reqs}` in
context/testfiles patterns. Component-local requirement files are unchanged — they
stay beside their component.

**The path rule** (one rule, everywhere): every path may be a glob; relative paths
start at the **project root** (the directory holding `.iter/`); `~` = home;
`{codepath}` anchors a pattern to the work item's own code. Codepath itself is always
stored absolute.

### 6. Test structure — `testgroup.iter.md` and `workitem.testfiles`

Tests live **with the code being modified** — the one data structure that must. Test
*groups* are containers over deterministic tests: each group has a defined launcher (one
or more scripts) that runs N tests and reports pass/fail.

There is one `testgroup.iter.md` per **component** — "component" per the C4 model
(context, container, component, code), where iterloop works mostly at the container or
component level. We don't go down to the code level; full end-to-end testing sits at the
project level. Typical placement:

```
project/
  subcomponent/
    src/
    test/
      testgroup.iter.md
      testscript01.sh
      testscript02.sh
      testscript03.sh
```

The file is ordinary markdown (minimally intrusive; drops into existing frameworks) with
a structured block in an HTML comment at the bottom (renders invisibly). The block is
JSONL — one group per line:

```markdown
# Whatever human-facing test documentation you want

...prose, how to run things, conventions, prompts for generating new tests in a group...

<!-- iterapp:testgroups
{"label":"auth tests",  "lastrun":"2026-08-10T14:02:11Z", "result":"passed", "counts":"24/24", "testlist":["testscript01.sh"]}
{"label":"authz tests", "lastrun":"2026-08-10T14:02:45Z", "result":"passed", "counts":"55/55", "testlist":["testscript02.sh","testscript03.sh"]}
{"label":"full tests",  "lastrun":"2026-08-10T14:03:30Z", "result":"failed", "counts":"75/79", "testlist":["testscript01.sh","testscript02.sh","testscript03.sh"]}
-->
```

Removing the block is treated as *never tested* and will likely trigger a retest sooner
than needed. Purposes: target specific work items at specific groups; hold prompts for
generating new tests within a group over time; persist last-run state for cheap reads.

**TDD, AI-centric:** the intended workflow is test-first — define tests, run them with a
test agent knowing they fail, then let a code agent use business requirements, technical
requirements, and common interfaces to plan/build/retest in a loop until tests pass. Once
green, add tests, cover more edge cases, and let the engine run continuously looking for
errors and anomalies.

## Work item schema

Stored one-per-line in `workitems.jsonl`. Logical schema (field names are normative;
serialization is plain JSON):

| field | type | set by | notes |
|---|---|---|---|
| `workid` | uuid string | engine (if absent) | stable identity across state changes |
| `title` | string | producer | short description, ~20–40 chars |
| `type` | string | producer | must match a basename in `.iter/agents/` (`plan`, `code`, `test`, `testwriter`, `refactor`, `ingest`, …) |
| `state` | enum | engine | `todo \| queued \| in-progress \| paused \| failed \| complete` |
| `source` | string | producer | `user` \| `agent: {type}` \| `error` |
| `priority` | int 0–10 | producer | 10 = most urgent |
| `risk` | int 0–10 | producer | 10 = highest risk; informational in v1 |
| `codepath` | path | producer | working directory for the agent; the lock scope |
| `codepath_ignore` | [string] | producer | gitignore-like patterns (relative to codepath) carved OUT of the lock scope (added 2026-08-15): the item must not touch those subtrees, so a parallel item can own them — e.g. code locks `pth/object/` ignoring `test/` while a testwriter locks `pth/object/test/`. A pattern with `/` is anchored to the codepath, one without matches at any depth; a matched directory excludes everything beneath it; glob wildcards ok. Stored in `.iter.lock` (`ignore`) so other acquirers see the carve-out |
| `context` | [glob] | producer | 0–N file searches, any local location (see §5) |
| `testfiles` | [path] | producer | `testgroup.iter.md` paths; handed to `test*` agents |
| `prework` | [string] | producer | step names from `prepostwork/`, or inline literal prompts; executed sequentially, in order |
| `mainwork` | string | producer | long prompt (often 1000s of chars) describing exactly the work |
| `postwork` | [string] | producer | same semantics as `prework`, after mainwork |
| `output` | string | engine | concatenation of all prework/mainwork/postwork agent outputs |
| `attempts` | int | engine | increments each time the item enters `in-progress` |
| `lasterror` | string | engine | populated when an attempt fails |
| `times` | object | engine | `{"added","start","preworkdone","mainworkdone","postworkdone","closed"}` — ISO-8601 UTC strings |

### State machine

```
              ┌────────────────────────────────────────────┐
              ▼                                            │ retry (backoff,
todo ──► queued ──► in-progress ──► complete ─► [closed]   │  attempts < max)
              ▲          │  │                              │
   user/agent │          │  └──► failed ───────────────────┘
   resume     │          ▼         │ attempts exhausted
paused ◄──────┴── (pause)          ▼
                                [closed]
```

- `todo` — defined but held for some reason; not eligible for pickup (reserved for
  dependencies/approval gates; v1 engine skips these). May be renamed `held` later if
  `todo` proves confusing — same meaning either way.
- `queued` — available to start.
- `in-progress` — record-locked by an agent run; `times.start` set, `attempts` +1.
- `paused` — skipped by the engine until returned to `queued` (by user or a work item).
- `failed` — attempt failed. Eligible for retry after `retry_backoff_sec` while
  `attempts < max_attempts`; otherwise terminal → moved to closed file. A retry is a
  **full re-run**: fresh session, prework, mainwork, and postwork all execute again.
- `complete` — moved (appended) to `workitems_closed.jsonl` and removed from the queue.

### Crash recovery
On startup, any item found `in-progress` is presumed orphaned (the engine that owned it
died): reset to `queued`, note in `lasterror`. Stale `.iter.lock` files (past their
timeout) are removed on discovery.

## Work-item handoff (agent-created work)

Work items are the standard, constant form of work-handoff between agents. Every agent
can queue new work items, and they will regularly do so. Canonical examples:

- **plan** — writes a parallelizable plan with tests, then creates 3–4 `code` work items
  and 2 `testwriter` work items to implement it.
- **test** (no tests found) — creates a `testwriter` work item to populate the missing tests.
- **test** (failures found) — fixes small/syntax issues directly; for larger problems,
  creates a `plan` work item.
- **ingest** — migrates an existing project onto iterapp; creates multiple `code` and
  `testwriter` work items to integrate and build missing files.

**v1 mechanism:** each agent's `.iter/agents/<type>.md` body includes standard handoff
instructions — create work items by running
`"$ITER_BIN" add --project "$ITER_PROJECT" --file <item.json>` with `source` set to
`agent: {type}`. The engine injects both env vars into every agent session:
**`ITER_BIN`** = the running executable's absolute path, **`ITER_PROJECT`** = the
project root that owns the queue — so handoffs are deterministic from any codepath,
with nothing on PATH and no cwd guessing. The agent is just another external producer
using the record-lock protocol; the engine notices the queue change on a later tick.
The long-term write path is TBD — the leading candidate is an iterapp-provided tool
served over **stateless MCP** (see Open questions).

**Guardrail:** `engine.max_open_workitems` caps the open queue. `iter add` refuses
new items at the cap (with a clear error the agent can report in its output), which
bounds runaway agent-creates-agent loops.

## Locking

Two independent locks, both file-based:

### Record lock (the queue file)
Guards writes to `workitems.jsonl`. Protocol: create `workitems.jsonl.lock` with
create-exclusive semantics (`O_CREAT|O_EXCL`); holder writes, then deletes the lock file.

- If another process holds it: wait `queue_lock_retry_ms` (50ms) and retry.
- Ghost-breaker: if the lock has been held for `queue_lock_break_sec` (60s) **and** the
  queue file's byte count and last-modified timestamp haven't changed over that window,
  force-delete the lock (holder is presumed dead). This mainly matters at startup, so the
  engine also staggers agent creation by `agent_stagger_ms` (100ms) to avoid a thundering
  herd on first tick.
- Within a single engine process, a mutex serializes queue writes; the file lock exists
  for external producers — chiefly agents creating handoff work items via `iter add`
  mid-run, plus the user's own CLI calls, a second engine, or a human script.

### Codepath lock (`.iter.lock`)
Guards the file tree an agent is editing, so two work items never modify overlapping file
sets concurrently. Before starting a work item the engine, per `codepath_lock.md`:

1. Checks the workitem's `codepath`, **all recursive subdirectories, and all ancestor
   directories** up to the filesystem root for any `.iter.lock` file that is still active
   (present and not past its embedded timeout). Ancestors matter: a lock at `project/`
   must block work in `project/subcomponent/`, and vice versa.
2. If an active lock is found → release the record lock (state back to `queued`, the
   attempt refunded) and return to Find Work. The blocked item is not re-picked for
   `codepath_conflict_backoff_sec` (default 15s), so a long-held lock doesn't churn the
   scheduler and the log every tick. The backoff is in-memory only — an engine restart
   simply retries sooner.
3. If none → write `.iter.lock` into the codepath root:

```json
{ "workid": "…", "agent": "code", "pid": 12345,
  "created": "2026-08-10T14:00:00Z", "timeout": "2026-08-10T15:00:00Z" }
```

Timeout = now + `codepath_lock_timeout_sec` (or the agent's `max_work_timeout_sec` if
larger). On completion or failure the engine removes the lock per `codepath_unlock.md`.
Locks past timeout are treated as absent and deleted when encountered.

## The engine loop

On `iter run`, per tick (`tick_interval_sec`):

1. **Load agents** — read `.iter/agents/*.md`; parse frontmatter (config) and body
   (prompt definition). Re-read each tick so edits apply live.
2. **Load queue** — reload `workitems.jsonl` only if size/mtime changed since last read.
3. **For each agent type with remaining capacity** (`running < max_agent_count`, and
   total running < `max_total_agents`), spawn a worker (staggered by `agent_stagger_ms`):

   **Find Work**
   - Select the best eligible item of this agent's type:
     - eligible: `state == queued`, or `state == failed` past backoff with attempts left
     - order by: `source == error` first (+2 effective priority), then `priority` desc,
       then oldest `times.added` first
   - Take the record lock; set `state = in-progress`, `times.start`, `attempts += 1`;
     save; release the lock.
   - Run the codepath-lock check/acquire (above). On conflict, revert and retry Find Work.

   **Spin-up**
   - Launch a headless Claude Code session (see [Agent runner](#agent-runner)) with the
     spin-up prompt composed of:
     - the agent definition body (`.iter/agents/<type>.md`)
     - the source instructions (`.iter/source/<source>.md`, `{type}` substituted)
     - the resolved `context` file list, the `codepath`, and — for `test*` agents — the
       `testfiles` list, with the instruction to read them before starting
   - The agent reads context as part of spin-up; the engine does not inline file bodies.

   **Processing** — sequential prompts into the same session, each allowed to finish
   before the next is sent:
   - each `prework` step (resolved per the prepostwork rule), in order → stamp
     `times.preworkdone`
   - the `mainwork` prompt → stamp `times.mainworkdone`
   - each `postwork` step, in order → stamp `times.postworkdone`
   - final check prompt: re-read `.iter/agents/<type>.md` and confirm every instruction
     was completed; report anything unfinished

   **Close-out**
   - Concatenate all step outputs into `output`.
   - Success → `state = complete`, `times.closed`, append to `workitems_closed.jsonl`,
     remove from queue. Failure/timeout → `state = failed`, `lasterror`; terminal
     failures close out likewise.
   - Remove `.iter.lock`. Log a one-line summary to the terminal.

4. Idle agent types sleep `sleep_interval_sec` before re-polling.

## Agent runner

v1 runs every agent headless via the Claude Code CLI. Sequential prompts reuse one
session via `--resume`, so the agent keeps full context across prework → mainwork →
postwork:

```bash
cd {codepath}

# first prompt (spin-up + first step): capture the session id
claude -p "<spin-up + step 1>" --output-format json --model {model} {model_flags}
#   → parse .session_id and .result from stdout JSON

# every subsequent step, same session:
claude -p --resume {session_id} "<next step>" --output-format json --model {model} {model_flags}
```

- `cwd` is the workitem's `codepath`.
- Each prompt turn is subject to `max_work_timeout_sec` (process killed on breach →
  attempt fails); spawn/handshake subject to `max_connection_timeout_sec`.
- stdout/stderr are captured per turn; `.result` strings are concatenated into
  `workitem.output` and streamed to the engine log.
- `model` and `model_flags` come from agent frontmatter, passed through verbatim.
- `llm_run_mode` is honored as `headless` only in v1; `terminal`/`tmux` values are
  accepted but treated as `headless` (reserved for the future watch-live modes).
- Permissions are owned by the target repo, not by iterapp: iterloop runs inside an
  existing repo, so the user's own `.claude/` settings (allow/deny rules, hooks) govern
  what agents may do. iterapp's only stance is a documented recommendation to remove
  `--dangerously-skip-permissions` from agent frontmatter before pointing at production
  code.

## Engine control

Work items can control the engine itself via prepostwork steps:

- `iterloop-stop.md` — the agent writes `.iter/.engine/stop.signal`; the engine finishes
  nothing new, terminates politely at the next tick boundary.
- `iterloop-wait-for-stop.md` — write the same signal, but the engine drains: no new
  Find Work, waits for all in-flight work items to finish, then stops.

`iter stop` from the CLI writes the same signal file. Deleting the file (or
`iter run`) clears the state.

## Terminal output (v1 observability)

v1's UI is the terminal. Every state transition logs one structured line:

```
14:02:11 INFO  [engine]      tick #42 — queue: 7 open (3 queued, 2 in-progress, 1 paused, 1 failed)
14:02:11 INFO  [code#2]      picked a1b2c3 "add auth middleware" (prio 6, source user)
14:02:11 INFO  [code#2]      codepath lock acquired: sample/
14:02:12 INFO  [code#2]      spin-up ok (session 9f8e…), context: 4 files
14:03:40 INFO  [code#2]      prework 1/2 done: git-pull
14:09:02 INFO  [code#2]      mainwork done (5m22s)
14:09:44 INFO  [code#2]      postwork 2/2 done: git-commit
14:09:44 INFO  [code#2]      complete → workitems_closed.jsonl; lock released
```

Logs also go to `log_default_path` with rotation per `log_max_size_mb` / `log_max_files`.
`log_level` controls verbosity (`debug` adds full prompt/response bodies).

## CLI (v1)

The executable is named **`iter`** — iterloop (the engine) and iterapp (the webapp)
are two functions of the one binary. The `.iter/` template is embedded in it;
`start`, `run`, and `init` all scaffold/heal missing `.iter/` files (never
overwriting existing ones), so deployment is copy-one-file.

```
iter start  [--project <path>] [--port N]   # engine loop + webapp server; prints the URL
iter run    [--project <path>] [--once]     # engine loop only (--once = single tick, for testing)
iter add    --file <item.json> | --type code --title "…" --mainwork "…" [--priority N] …
iter status [--project <path>]              # queue summary + active agents + locks
iter stop   [--wait]                        # write stop.signal (--wait = drain first)
iter init   <path> [--from <dir>]           # scaffold .iter/ from the embedded template (idempotent)
```

`add` appends to `workitems.jsonl` under the record-lock protocol — it is the reference
implementation of an *external producer*, and the v1 write path agents use for handoff.
It **warns** (not errors) when `--type` doesn't match a file in `.iter/agents/` — the
engine enforces at pick time — and it **errors** when the open queue is at
`max_open_workitems`.

## Deployment

- **v1:** single Rust binary, run locally on a laptop or server. No daemon, no network.
- **Later:** the product ships as the Rust engine **plus a small web server** presenting
  a localhost URL for maintenance and investigation.
- **Cloud:** decision deliberately parked. A lightweight Linux container or a very small
  EC2 instance would both serve; `--once` keeps a single tick self-contained regardless,
  so nothing in v1 blocks on this.

## Future (v2+)

Deliberately excluded from v1, but the file formats above leave room:

- Scheduling intelligence: dependency-aware ordering, splitting oversized items, smarter
  prioritization. (Agents *creating* work items is already core v1 behavior.)
- An iterapp-provided work-item tool for agents — likely stateless MCP — replacing the
  `iter add` convention; the same interface would back the web UI.
- Web UI (engine + local web server) for queue maintenance and investigation.
- `.sh` prepostwork steps and the `.md`+`.sh` paired pattern (AI decides → script executes).
- `llm_run_mode: terminal | tmux` for watch-live agent sessions.
- `todo` state semantics: dependencies between work items, human approval gates.
- Finer-grained locking (glob-scoped locks instead of whole-codepath).
- Test generation loops driven by `testgroup.iter.md` group prompts; `testwriter_min_tests_per_group`/`testwriter_max_tests_per_group`
  enforcement from config.
- `risk`-aware scheduling and approval policies.

## Open questions

1. **Agent write path, long-term:** v1 agents create work items via the `iter add`
   convention. Is an engine-served **stateless MCP** tool the right long-term interface
   (cf. Google's stateless-MCP infrastructure writeup:
   <https://developers.googleblog.com/scaling-ai-agent-infrastructure-with-the-mcp-stateless-updates/>)?
   It would decouple agents from binary paths and double as the web UI's backend.
2. **Handoff guardrails beyond `max_open_workitems`:** dedup of near-identical items
   (same type + codepath + similar title)? A lineage/depth cap on agent-created chains?
3. **Cloud packaging:** parked by choice — container or a very small EC2 both serve;
   revisit after v1.

---

# Build plan

Phased so every phase ends runnable and verifiable in the terminal. Phases 1–2 have no
code dependency and can proceed in parallel with 3+.

## Phase 0 — Repo scaffolding & hygiene
- [x] `cargo init` — binary crate `iterloop`; module skeleton: `config`, `workitems`,
      `agents`, `locks`, `runner`, `scheduler`, `cli`, `logging`.
- [x] Rename `src/.iter/source/agent ({type}).md` → `src/.iter/source/agent.md` (and the
      `sample/.iter/` copy).
- [x] Move this spec's decisions into stubs: fix `sample/test/testgroup.iter.md`
      structured block to valid JSONL with the `iterapp:testgroups` marker.
- [x] `.gitignore`: add `target/`, `*.iter.lock`, `.iter/.engine/stop.signal`, logs.

## Phase 1 — Fill in all template content (`src/.iter/**`)
Every stub becomes real, spec-compliant content. Frontmatter per §1; bodies written as
complete, usable prompts.

**`src/.iter/agents/`**
- [x] Shared handoff block, included in every agent body: when and how to create new work
      items (`iter add --file <item.json>`, `source: agent: {type}`, priority/risk
      guidance, respect the `max_open_workitems` error).
- [x] `plan.md` — frontmatter (`max_agent_count: 1`, model opus); body: read bizreq/techreq
      + context, produce a **parallelizable** plan with tests and acceptance criteria, then
      create the follow-on work items (typically 3–4 `code` + 2 `testwriter`).
- [x] `code.md` — expand existing stub body into a full coding-agent definition:
      TDD-first behavior, respect common interfaces from context, no scope creep,
      report changed files.
- [x] `test.md` — (`max_agent_count: 2`) run test groups from `testfiles`, parse results,
      update the `iterapp:testgroups` block lastrun/result/counts, report failures
      precisely. No tests found → create a `testwriter` work item; failures → fix
      small/syntax issues directly, create a `plan` work item for larger problems.
- [x] `testwriter.md` — generate new deterministic tests within an existing group per the
      group's generation prompt; respect `testwriter_min_tests_per_group`/`testwriter_max_tests_per_group`.
- [x] `refactor.md` — behavior-preserving changes only; tests must pass before and after.
- [x] `ingest.md` — read external requirements (e.g. `sample/bizreq.md`, `techreq.md`)
      and normalize them into context markdown the other agents consume; when migrating
      an existing project onto iterapp, create the `code` and `testwriter` work items
      needed to integrate and build missing files.

**`src/.iter/prepostwork/`**
- [x] `git-pull.md` — pull/rebase, abort the work item cleanly on conflict.
- [x] `git-commit.md` — stage related changes, commit-message convention, no unrelated files.
- [x] `git-push.md` — push current branch; report remote state.
- [x] `git-pr.md` — open PR via `gh`, PR body convention, return URL.
- [x] `deploy.md` — placeholder deterministic-ish deploy instructions (real deploys are
      the future `.md`+`.sh` pair; say so in the file).
- [x] `iterloop-stop.md` — write `.iter/.engine/stop.signal`, confirm, do nothing else.
- [x] `iterloop-wait-for-stop.md` — same signal, drain semantics documented.

**`src/.iter/source/`**
- [x] `user.md` — trust the request as stated; ask-nothing autonomy rules; how to handle
      ambiguity (choose + document).
- [x] `agent.md` — work created by `agent: {type}`; verify the originating agent's
      assumptions before building on them.
- [x] `error.md` — reproduce first; skeptical of the reported description; fix root cause,
      add a regression test.

**`src/.iter/.engine/`**
- [x] `config.json` — extend to the full §4 schema (engine block + globalsettings).
- [x] `codepath_lock.md` — lock-acquisition procedure + `.iter.lock` JSON template (per §Locking).
- [x] `codepath_unlock.md` — release procedure + stale-lock rules.
- [x] `workitems.jsonl` — 3–4 seed example items (one per major type) exercising every
      field, targeting `sample/`.
- [x] `workitems_closed.jsonl` — one worked example of a closed item (documents the shape).

**`sample/` (mock target project)**
- [x] Sync `sample/.iter/` from the finished `src/.iter/` template.
- [x] `sample/bizreq.md` + `sample/techreq.md` — small real requirements for a toy feature
      (enough for an end-to-end demo).
- [x] `sample/test/` — make `testscript01.sh`(+02, 03) real, trivially-passing scripts
      referenced by `testgroup.iter.md`.
- [x] Replace `sample/src/codefile.txt` with a minimal real code file the toy feature touches.

## Phase 2 — Data layer (Rust)
- [x] `config`: load/validate `config.json`, defaults for missing keys.
- [x] `agents`: discover `.iter/agents/*.md`, parse frontmatter + body; unknown keys ignored.
- [x] `workitems`: JSONL read/write; serde model of §Work item schema; state-machine
      transitions as methods; move-to-closed append.
- [x] `context`: glob resolution with `{codepath}`/`~` substitution; deterministic, fast;
      missing-path warnings.
- [x] `testgroups`: find/parse/update the `iterapp:testgroups` JSONL comment block.
- [x] Unit tests for every parser against the Phase-1 template files (the templates ARE
      the fixtures).

## Phase 3 — Locking
- [x] Record lock: create-exclusive lockfile, 50ms retry, 60s ghost-breaker
      (size+mtime heuristic), in-process mutex.
- [x] Codepath lock: ancestor + descendant `.iter.lock` scan, acquire/release, timeout
      handling, stale cleanup.
- [x] Startup recovery: orphaned `in-progress` → `queued`; expired locks removed.
- [x] Tests: two threads/processes contending for the queue; overlapping codepaths
      (parent/child) refusing to double-lock; ghost-break firing.

## Phase 4 — Agent runner
- [x] Spawn `claude -p --output-format json` with cwd/model/flags; parse `session_id`/`result`.
- [x] Sequential turns via `--resume`; per-turn timeout kill (`max_work_timeout_sec`),
      connection timeout.
- [x] Prompt composer: agent body + source instructions ({type} substitution) + context
      list + codepath (+ testfiles for `test*`).
- [x] Fake-runner mode (env var swaps `claude` for a stub script echoing canned JSON) so
      the loop is testable without burning tokens.

## Phase 5 — Scheduler loop
- [x] Tick loop: agent reload, queue change-detection reload, capacity accounting,
      100ms stagger, `sleep_interval_sec` idle behavior.
- [x] Find Work: eligibility + ordering (error-source boost, priority, age), retry
      backoff, attempts cap.
- [x] Full lifecycle wiring: Find Work → locks → Spin-up → Processing (pre/main/post +
      final self-check turn) → Close-out; timestamps at each stage.
- [x] stop.signal handling (immediate + drain).
- [x] Structured terminal logging + rotating file log per config.

## Phase 6 — CLI + end-to-end demo
- [x] `iter run|add|status|stop|init` per §CLI (`add`: warn on unknown type, error at
      `max_open_workitems`).
- [x] E2E (fake runner): seed 4 items across types into `sample/`, run engine, verify
      concurrency caps, lock contention on shared codepath, closed-file archival, output
      concatenation, timestamps.
- [x] E2E handoff (fake runner): a fake agent turn invokes `iter add` mid-run; verify
      the new `agent: {type}`-sourced item lands in the queue and is picked up on a later
      tick; verify the `max_open_workitems` refusal path.
- [x] E2E (one real item): a real `code` workitem against `sample/` with real
      `claude -p`, watched in the terminal.
- [x] README: quickstart (init → add → run), file-format reference pointing at this spec.

**Definition of done (v1):** `iter run` against `sample/` executes seeded work items
concurrently within capacity limits, no two agents ever hold overlapping codepath locks,
every lifecycle transition is visible in the terminal, and closed items land in
`workitems_closed.jsonl` with full `output` and `times`.
