# Feature: usecase agent

Status: DESIGN LOCKED 2026-08-17 — engine mechanisms BUILT same day (reject
verb, markers dump, participants CLI, ITER_USECASE_DIR, sweep authoring items,
priority inversion; second round: use-case/interface testgroups in the sweep
universe, contract enforcement, the non-convergence guard); agent definition
shipped as `.iter/agents/usecase.md`.

A **use-case** is a high-level end-to-end (E2E) workflow: one discrete action
taken by some actor within the codebase's scope. For a Netflix-like codebase:

- webpage user successfully authenticates with username/password
- webpage user successfully authenticates with OAuth and Google
- webpage user authentication fails
- logged-in user is shown their customized home page
- user clicks a movie they want to watch, and the movie starts
- user scrolls down to see extended options of shows to watch
- user clicks "play games" and the game page appears

Use-cases (typically) avoid technical instructions. Almost every use-case above
is silently supported by authorization, service-mesh mTLS between services,
running containers, and so on — but those details don't matter to the user's
experience of simply logging in, so they don't belong in the use-case.

## Why use-cases (two goals)

1. **Understanding how actors traverse the technical environment.** The project
   is a hierarchy of largely technical C4 objects — context (data,
   communications, infrastructure, decision, …), container (abstracted compute:
   docker container, library, OSS project), component (specific compute:
   library, package, crate, dll); code-level is not managed or visualized
   today. A use-case layers ACROSS N contexts, with containers and components
   invisibly supporting it. Knowing which technical elements support which
   use-cases shows what is heavily vs sparsely used and lets us do feature ROI
   in terms of user impact.
2. **User-centric TDD.** Use-cases guide development: "most important" is
   defined by the user's journeys, so the loop hammers the high-use, highly
   visible use-cases green first and expands carefully from there.

## Decisions locked 2026-08-17 (discussion with owner)

- **Agent name is `usecase`** (single lowercase word, matching code/plan/
  testwriter). Lock scope = the use-cases directory only (`$ITER_USECASE_DIR`,
  from `globalsettings.usecase_default_path`, default `{codepath}/usecases/` —
  e.g. `~/dev/pdy-dev/usecases/` on pdy-dev). Reads anywhere, writes only
  use-case files; never blocks code work.
- **Rejection is a first-class verb, for EVERY agent — `iter reject`.**
  Rejected work must be neither *complete* (too big a bucket; rejections would
  be swallowed and never seen again, and "complete" implies no more work to do,
  which is false) nor *failed* (retries would re-derive the same rejection).
  `iter reject --reason "…"` writes a flag file (same pattern as critfail); the
  engine consumes it at the turn boundary and moves the item to **`todo`** —
  the low-volume, high-attention bucket — with the reason in `lasterror` and
  the agent's analysis in `output`. Todo items are editable and requeueable, so
  "pause and edit, then resubmit" is the natural re-evaluation path.
- **One plan item covers ALL of a use-case's gaps** (not one per missing
  object): the missing objects of one use-case interrelate — shared interfaces,
  build order — and a single plan keeps those seams coherent. Build parallelism
  is not sacrificed: the plan agent already decomposes into N parallel
  code/testwriter items (born `todo`, the human review gate).
- **usecase↔C4 links reflect what was BUILT, not what was proposed.** The plan
  agent knows what it proposes; the user may change things between marker
  creation and code completion. So linking is a post-process through a
  deterministic, engine-owned CLI for global objects: `iter usecase --file <f>
  --add "<step> <ref>"` edits a use-case file's ordered `participants:` list at
  the record level, callable from ANY agent regardless of lock scope (the same
  spirit as the testwriter's sanctioned marker-key registration).
- **C4 traversal is deterministic**: `iter markers` dumps the scanned tree as
  JSON — the same scan (scan_roots + marker_glob, ~ expanded) the webapp,
  sweep, and validate share. Agents never re-implement discovery with shell
  globbing. Requirements checks read BOTH the global `*bizreq.iter.md` /
  `*techreq.iter.md` (in `$ITER_REQS`, auto-surfaced to every item) AND the C4
  object's local ones (declared in its marker frontmatter).
- **Priorities inverted project-wide** (same decision, same day): LOWER =
  sooner, P0 most urgent, default 5. `iter invert-priorities` migrates an
  existing open queue (newP = 10 - P).
- **Use-cases and interfaces are sweepable units** (second round, same day):
  both declare `testgroup:`/`test_dir:` keys like markers. For these two
  kinds a MISSING key is a coverage gap (the sweep births a testwriter
  authoring item in todo) because tests are their whole point — use-cases get
  E2E journey tests, interfaces get contract-enforcement tests asserting real
  provider I/O against the contract's example (interfaces become enforcement,
  not documentation). `testgroup: none` is the explicit opt-out. Red runs span
  C4 objects, so their fix items scope to the code root (usually todo — a
  human can narrow the codepath) with diagnose-or-escalate guidance. New
  use-cases declare their E2E testgroup at creation with empty testlists, so
  the sweep's authoring flow fills them.
- **New-WorkItem defaults live on agent defs** (2026-08-18):
  `default_codepath` / `default_codepath_ignore` frontmatter keys, with
  `{usecase_dir}`/`{interface_dir}`/`{test_dir}` placeholders resolved
  per-project by the server; the form pre-fills them when a type is picked and
  never clobbers user-typed values. The usecase agent ships
  `default_codepath: {usecase_dir}`, `default_codepath_ignore: {test_dir}/` —
  lock the usecases tree, carve every per-usecase test dir for parallel
  testwriters.
- **Non-convergence guard, 3rd lap** (owner call: allow two laps): escalated
  plans carry `--source-testgroup`; `iter add` counts plans per testgroup and
  holds the third in `todo` with a NON-CONVERGENCE note — a human breaks the
  fix→plan→build→still-red loop instead of it grinding forever.

## The usecase agent's flow

Handed a use-case idea from the user, the agent:

1. **Validates** it, rejecting (via `iter reject`, above) when the idea is:
   - out of scope for the project ("order food for delivery" on Netflix);
   - unclear in its goals ("hit button");
   - overly technical ("run database query ABC");
   - in violation of a bizreq invariant ("user successfully logs in with a
     1-character password").
   The rejection spells out WHY and what the user might change to fix it; the
   user re-evaluates the todo item (edit + requeue), or deletes it.
2. **Creates the use-case as a FOLDER** (layout decision 2026-08-18, mirroring
   PDY-TECH-030's folder-owns-its-files law for C4 objects):

       $ITER_USECASE_DIR/<short-name>/
         <short-name>.usecase.iter.md    ← declaring file: name/description/participants + narrative
         <test_dir>/testgroup.iter.md    ← E2E groups declared, testlists empty (sweep fills via authoring items)

   The `testgroup:`/`test_dir:` keys on the usecase file use the project's
   `test_dir` name (`tests` on pdy-dev, `test` by default). **NO marker file**
   in usecase folders: markers declare C4 NODES, and a use-case is the overlay
   ACROSS nodes — a marker here would mint a phantom object in the hierarchy
   and double-sweep the folder. The `*usecase.iter.md` file is the declaring
   file; its filename carries the role.
3. **Identifies required C4 objects** from `iter markers` + the reqs docs
   (e.g. "User Auth" implies an API gateway and an auth key strategy — those
   architecture decisions **should** live in the global techreq/bizreq).
4. **PRESENT objects** → referenced into the use-case immediately, as ordered
   `participants:` entries (`- <step> <object-ref>`, the format the UI's
   overlay map draws from).
5. **MISSING objects** → one plan item (P3, queued) listing every gap:
   - plan agent builds the plan honoring bizreq/techreq/interfaces, then (its
     normal TDD behavior) creates the code + testwriter items in `todo`;
   - **human review gate**: user reviews the plan and the todo items, updates
     as needed, then flips them to `queued` — code & interfaces and testgroups
     & tests are then built in parallel from the same docs;
   - each code item, on completion, links its object back into the use-case
     via `iter usecase --add` (instruction carried in the plan mainwork).

## How the TDD test loop connects (same 2026-08-17 flow decision)

The Test Loop sweep (see TDD.md) walks all declaring files — markers,
use-cases, AND interfaces — and runs all their testgroups:

- **no `testgroup:` key on a use-case/interface** → coverage gap: ONE
  testwriter authoring item in `todo` (create the testgroup file, register the
  key — the testwriter's sanctioned outside-write — and author the tests).
  Markers keep the old rule: absence is a choice.
- **testgroup/tests MISSING or empty** → the sweep births ONE testwriter
  authoring item in `todo` (dedup by `source_testgroup`). Minor effort (tests
  only): the testwriter authors and registers them; they run next sweep cycle.
  Major effort (the CODE is missing too): the testwriter escalates to a plan
  item with its gap analysis and `--source-testgroup` — plan → human gate →
  parallel code/testwriters — mirroring the code agent's escalate-to-plan. The
  deterministic sweep never judges minor vs major; the agent that looked does.
- **tests PRESENT but red** → one fix item per group (auto_fix gates
  queued/todo). Object groups scope to the object's directory; use-case
  journey and interface contract groups scope to the code root with
  diagnose-or-escalate guidance. Simple fixes land directly, structural
  defects escalate to plan with the same human gate — and the non-convergence
  guard holds the third plan for the same group in todo.

## Use-case UI

Unchanged from current state: use-cases are selectable in the UI and draw an
overlay map over the C4 view showing which objects are used, in what order
(from `participants:`), for every use-case.

## Storage

`globalsettings.usecase_default_path` (default `{codepath}/usecases/`) is where
NEW use-case files are created — creation only; the scanner finds
`*usecase.iter.md` anywhere. Exported to agents as `$ITER_USECASE_DIR`.
