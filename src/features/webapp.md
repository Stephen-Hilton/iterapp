# Iterloop Web App — Feature Specification (v1: Work Items page)

A local web interface wrapped around the iterloop engine. One executable serves both:
the engine loop and a small web server presenting a localhost URL for maintenance and
investigation (per the deployment note in [iterloop.md](iterloop.md)).

Source mockup: Google Slides, slide 1 ("IterLoop / Work Items"). Slide 2 ("IterApp /
Projects" — the C4 project hierarchy with use-case filtering) is a future page; it is
inventoried here but **out of scope** for this build.

## Priorities (locked 2026-08-11)

1. Easy to use, intuitive, highly interactive.
2. Information dense but fast to understand — important details legible at a glance.
3. Big summary buttons on top double as filters when clicked.
4. Work-item detail slides down in place when a row is clicked.
5. Iterate on a standalone HTML mockup first (`src/webapp/mockup.html`); merge into the
   Rust binary only after the design settles.

## Page inventory

| Page | Status |
|---|---|
| **Work Items** (slide 1) | **specified below; mockup complete** |
| **Dashboard** | **initial version specified below** — queue history, productivity metrics |
| **Iterloop Settings** | **initial version specified below** — edit engine/global config |
| **Projects** (slide 2 / Code Canvas visual) | **initial version specified below** — marker-file project model |
| **Project Settings** (ITERAPP → Settings) | **initial version specified below** — scan roots, marker globs |

## Layout — Work Items page

### Chrome
- **Header**: `IterLoop / Work Items` title, left. Right: **engine state chip**
  (`Running` / `Paused` / `Stopped` / `Draining`), colored, always visible. Clicking it
  offers the engine controls (pause = write `stop.signal` drain; resume = clear).
- **Left sidebar**: two nav groups — ITERLOOP (Dashboard, Work Items, Settings) and
  ITERAPP (Projects, Settings). Current page highlighted. Collapsible on narrow screens.

### Summary buttons (top strip)
Seven large colored buttons, each showing label + live count:

| Button | Color | Filter |
|---|---|---|
| Total | purple | all items (open + closed) |
| ✅ Complete | green | `complete` (from closed file) |
| ⚙ In-Progress | amber | `in-progress` |
| ⏱ Queued | blue | `queued` |
| 📋 ToDo | pink | `todo` |
| ✖ Failed | red | `failed` (open retry-eligible AND terminally closed) |
| ⏸ Paused | orange | `paused` |

Behavior: click = toggle that state filter (multi-select; Total resets to all). Active
filters render pressed/inset. Counts update live. Below the strip, right-aligned:
**Sort selector**, **Collapse All**, and **New WorkItem**.

Sort options: **State** (default: lifecycle groups, effective priority within),
**Date Requested** (newest first), **Date Completed** (incomplete on top, then newest
completion first), **Priority** (effective, error-source boost included), **Agent
Assigned** (type), **Requested By** (source).

### Work-item row (collapsed)
One dense two-line row per item, ordered by state group then priority. Left to right,
top to bottom (exact order locked):

1. **Big state icon** — instant read of overall state (✅ ⚙ ⏱ 📋 ✖ ⏸), colored block
   spanning both lines.
2. **Title** — across the entire top line, single line, ellipsized.
3. Badge row: **state** (colored pill) · **agent type** that does/did the work (`Code`)
   · **P:7** priority · **R:3** risk · **source** entity that requested it (`Plan`
   agent, any agent, or `user`) · **T:98/98 – 100%** test results as X/Y and percent ·
   **A:1** attempts · **Requested:** ISO-8601 creation datetime · **ID:** last segment
   of the UUID.
4. **Actions** button — right edge, spans both lines, opens the context-aware menu.

Row click (anywhere except Actions) toggles the slide-down detail.

### Actions menu (context-aware by state)

| Condition | Action | Effect |
|---|---|---|
| `in-progress` | **Clone only** | the engine is loosely coupled to the queue: while an agent runs the item, queue-side Complete/Pause/Delete wouldn't change engine behavior, so the menu offers only Clone (with a note explaining why) |
| `todo` or `paused` | **Queue** | state → `queued` |
| not `complete` / not `in-progress` | **Complete** | state → `complete`, close out |
| not `complete` / not `in-progress` | **Pause & Edit** | state → `paused`, opens the edit lightbox; remembers the prior state as the save default |
| `failed` | **Requeue (Retry)** | state → `queued` *(suggested addition — one-click retry instead of Pause→Queue; cut if unwanted)* |
| `complete` | **Create Follow-up Request** | opens New WorkItem form pre-filled: same codepath/context/testfiles, `source` = user, title "Follow-up: …", empty mainwork |
| any | **Clone** | duplicates every request element, new workid, state `todo` |
| any | **Delete** | removes the work item (confirm dialog; hard delete from jsonl) |

### Slide-down detail (expanded row)
Slides open beneath the row; multiple rows may be open at once (Collapse All closes
everything). Contents, top to bottom (mirrors the mockup):

- **CodePath** — monospace, read-only in view mode, **always displayed absolute**.
  Users may enter a relative path in the form; the UI (and later the API) resolves it
  against the project root and stores it absolute — for clarity first, safety second.
  `context` and `testfiles` stay relative (they read against the visible codepath),
  unless they point outside it (e.g. a project-central or machine-central assignment).
  Right-aligned: **full UUID**.
- **Prework** — the full pool of `.iter/prepostwork/*` names rendered as pill toggles;
  assigned steps highlighted, unassigned dimmed. View mode: read-only. Inline literal
  prompts (not matching a file) render as an extra pill with a tooltip.
- **Request** — the `mainwork` prompt, full text. Grows with content (both Request and
  Output can run hundreds of lines) up to ~60% of the viewport, then scrolls — never a
  two-line peephole.
- **Postwork** — same pill treatment as Prework.
- **Output** — the concatenated agent output, scrollable, monospace.
- **Details buttons** → lightboxes:
  - **View Test Details** — per-group results from the item's `testfiles`
    (`testgroup.iter.md` blocks): group label, lastrun, result, counts, scripts.
  - **View Time Records** — the `times` object as a timeline (added → start →
    preworkdone → mainworkdone → postworkdone → closed) with computed durations.
  - **View Context and Prompts** — `context` patterns + resolved files, source
    instructions applied, and the composed turn sequence (labels + prompts).
  - **View Logs** — engine log lines tagged with this item's worker (`[code#N]`).

### New WorkItem / Edit form
Same lightbox for create, edit (Pause & Edit), clone, and follow-up — identical layout
to the slide-down detail, except **(A)** every field is editable and **(B)** the header
badges become an editable section: title, type (agent selector from `.iter/agents/`),
priority + risk sliders (0–10), source (defaults `user`), codepath, codepath_ignore
(one gitignore-style pattern per line, relative to codepath — subtrees carved out of
the item's lock scope so parallel items can own them; added 2026-08-15), context
patterns (one per line), testfiles. Prework/Postwork pills toggle on click; a
free-text row appends inline literal steps. One form, one footer for every mode:
`Cancel · [Create|Save] and set to · [Queued | ToDo | Paused]`. The selector chooses
the state the item lands in; only the default differs — create/clone/follow-up default
to **Queued**, Pause & Edit defaults to the item's state **before** it was paused
(falling back to Queued if that state isn't a pre-processing one, e.g. failed). Output,
times, and attempts are never editable.

### The path rule (locked 2026-08-11)

One rule everywhere a path appears — work-item `context` and `testfiles`,
`scan_roots`, `default_context`:

> **Every path may be a glob. Relative paths start at the project root (the
> directory holding `.iter/` — where `./iter start` ran). `~` = home.
> `{codepath}` anchors a line to the work item's own code.**

One storage exception: **codepath itself is always stored absolute** (enter it
however you like; the UI resolves and shows the result before you save). A blank
New WorkItem prefills its context from `default_context`'s literal lines —
nothing is hardcoded; Create-from-node expands the full template (placeholders
included). The rule is printed in the form itself so nobody has to guess.

## Other pages (initial versions, locked 2026-08-11)

All pages share the chrome (sidebar, engine chip) and live in the same single-page app;
the sidebar routes client-side (`#/dashboard`, `#/workitems`, …).

### Dashboard

High-level queue history and productivity metrics. Everything derives from
`workitems_closed.jsonl` (+ the open queue for "now" numbers) — no new storage.

- **Stat tiles**: Success rate (7d, complete ÷ closed), Completions/hour (7d avg),
  Median cycle time (added → closed), Active agents now, Open queue depth, Handoff
  share (% of closed items with `agent:` source — how self-directed the loop is).
- **Trend window** (locked 2026-08-11): selector offering **7 / 14 / 30 days**
  (default 14) plus **custom start/end date** pickers; all panels share the window.
- **Completions per day** — bar panel over the selected window.
- **Failures per day** — its own small panel under the same x-axis. *Deliberately not
  stacked with completions: the green/red pair fails colorblind-separation checks when
  adjacent (validated, deutan ΔE 2.8) — small multiples read correctly for everyone.*
- **Completions by agent type** — horizontal bars, last 7d, direct value labels.
- Hover tooltips on all bars; each chart offers a table view for accessibility.
- API: `GET /api/history?days=N` returns per-day buckets the server computes from the
  closed file.

### Iterloop Settings

A form over `.iter/.engine/config.json`, one field per key, grouped **engine** vs
**globalsettings**, with the same defaults the engine uses. Save writes the file
atomically (same tmp+rename as the queue); the engine picks changes up on its next
tick (agents/config are re-read per tick already). Numeric fields validate ranges;
unknown keys found in the file are preserved untouched.

### Projects

Model and maintain large project codebases that iterloop builds from — the Code
Canvas visual (hierarchy rows, colored level chips, use-case thread), but with a
decisive data-model difference: **content lives in distributed in-code marker files,
not a central definition file**, and the page is **loosely coupled to the work-item
queue** — its one write-path into execution is submitting work items.

**Marker files.** One naming convention: **any `*.iter.md` file is
iterapp-meaningful**; its **role comes from its frontmatter**, not its filename:

| Frontmatter | Role |
|---|---|
| has `level:` | **structure node** — a row on the Projects map |
| has `interface:` | **interface contract** — aggregated globally (see below) |
| has `participants:` | **use-case thread** — drives the red line + step numbers |
| none | **plain context document** (testgroup.iter.md, bizreq.iter.md, notes) — discoverable and attachable as context, never a map node |

*(Naming note, locked 2026-08-11: an earlier draft used `*.iter.context.md`, where
"context" meant generic agent-context — too easily read as the C4 CONTEXT level. The
level lives in the file's frontmatter, so the filename doesn't need it.)*

A node exists because a marker file exists near the code it describes, e.g.
`./src/some/path/some_file_name.iter.md`:

```markdown
---
name: Evidence Vault
level: component          # free-form label; project/context/container/component suggested, not enforced
description: "10-word summary shown in the hierarchy row"
uses: [postgres, vault]   # shared resources / interfaces, shown as badges
---
Free markdown body: THE context handed to agents working under this node —
requirements, interfaces, constraints. The marker IS the context file.
```

- **Hierarchy is derived, not declared**: nearest-ancestor-marker by directory nesting
  builds the tree — there is no parent-override key, and none is planned. Cross-cutting
  relationships are edges, not parentage: they go through interfaces (`uses:` /
  `provides:`) and use-case `participants:`, both of which already cross the tree
  freely. This keeps the data distributed AND decouples structure from a strict C4
  hierarchy — nest contexts in contexts, skip levels, invent levels; the page renders
  whatever depth exists.
- **Levels are free-form, C4 by default** (locked 2026-08-11): out of the box the
  vocabulary is `project / context / container / component` — zero configuration, and
  that's the whole story for most users. Advanced users may define custom levels
  (name + chip color, ordered) in Project Settings; any `level:` value not in the
  list still renders, with a neutral chip. Explaining it stays one sentence: *"use
  the C4 names, or define your own list in Project Settings."*
- **Read-only by design** (locked 2026-08-11): the page never edits marker files.
  Changing the model = a work item that edits the marker — the loop maintains its own
  map. The one write path into execution is Create WorkItem.
- **Interfaces are first-class and global** (locked 2026-08-11): an interface is a
  contract between two-or-more heterogeneous systems — the stitching BETWEEN nodes —
  so it is never owned by the hierarchy. A marker with `interface: <id>` frontmatter
  (plus `kind:` http|grpc|kafka|sql|cli|library|…, `endpoint:`, `description:`; body =
  the contract) can live anywhere; the scanner aggregates all of them globally so they
  can be rationalized and deduplicated (duplicate ids are flagged loudly). Nodes
  *reference* interfaces: `provides: [id…]` for the serving end, and entries in
  `uses:` that match a declared id become links (unmatched entries stay plain resource
  badges — the "used but undeclared" worklist). Two things fall out free: clicking an
  interface threads its providers/consumers through the tree — a **derived thread**
  nobody had to author — and Create-WorkItem-from-node attaches the contract files for
  everything the node uses/provides (the `{interfaces}` placeholder). iterloop itself
  stays ignorant: interfaces are just more context for prompts.
- **Use-cases are `*.iter.md` files with `participants:`**: a name, description, and
  an ordered participant list (`- 2.1 core/intake/orchestrator`; `.` = the project-root
  node). The red thread and step numbers render from it; structure files never mention
  use-cases.
- **Use-Cases section with CRUD (added 2026-08-15)**: the Projects page lists every
  discovered use-case below Interfaces — expandable rows (file, participants with
  resolved node names and unknown-key flags, story body) where expanding also threads
  the red line, plus New/Edit/Delete. Created files land in
  `globalsettings.usecase_default_path` (default `{codepath}/usecases/`) as
  `<slug>.usecase.iter.md` — creation only; the scanner finds `*usecase.iter.md` anywhere;
  edits rewrite the file wherever it lives; the API (`POST/PUT /api/usecases`,
  `POST /api/usecases/delete`) refuses paths outside the project and files that aren't
  use-case markers. A project whose scan finds zero use-cases is seeded ONCE (flag:
  `.iter/.engine/usecases_seeded`) with the starter "Install iter framework" — the
  getting-started story of iterapp itself; deleting it is a real delete, not a respawn.
- **Discovery by search**: the engine scans configured roots for the single marker
  glob (default `**/*.iter.md`), sorts each hit into node / use-case / plain-context
  by frontmatter, and caches to `.iter/.engine/markers.json`; a **Rescan** button
  (and later a file-watcher) refreshes. Ultimate flexibility: adding a node to the
  model = dropping a file in the tree, same extensibility rule as `.iter/` itself.
- **Queue coupling (loose, one-way)**: every node row offers **Create WorkItem** —
  opens the standard form prefilled with `codepath` = the marker's directory and
  `context` = the marker file plus its ancestor chain. The marker body becomes agent
  context automatically; processing then flows through the normal queue.
- Page furniture: level-chip legend (doubles as depth filter), order-by
  (Hierarchy | Level | Name), use-case filter (participants highlighted with step
  numbers, non-participants dimmed), per-row expand with marker path / codepath /
  uses / View Marker lightbox.
- API: `GET /api/markers`, `POST /api/markers/rescan`.

**Why markers beat a central file here** (suggestions, adopted in this spec): the
marker doubles as the agent-context document, so the model and the prompts can't
drift apart; `testgroup.iter.md` naturally sits beside its component's marker,
aligning the test tree with the model; and git ownership of a node follows the code
it describes (a component team owns its marker like its code).

### Project Settings (ITERAPP → Settings)

Kept — it earns its place as the home of marker discovery config: project name,
**url_slug** (hostname + branding: `{slug}.localhost:{port}`, tab title, favicon
tint), **scan roots** (multiple; can point outside the repo), the single **marker glob**
(default `**/*.iter.md`; role sorted by frontmatter, see Projects), the
**testgroups glob**, **default_context**, and (advanced) **custom level
definitions** — ordered `{name, color}` list overriding the C4 default vocabulary.
Stored in `.iter/projects.json` (extensible area — user-editable, engine-read).

**`default_context`** is the template for the `context` list prefilled when you
Create WorkItem from a Projects node — one entry per line; placeholders expand at
form-open time, and every line supports the same globs as any work-item context:

| Entry | Expands to |
|---|---|
| `{marker}` | the clicked node's own `.iter.md` file |
| `{ancestor_markers}` | the marker files of that node's parent chain, walking up to the project root — strictly directory-derived: chop the node's key at each `/` and take whatever marker sits at that key, so a directory with no marker contributes nothing. E.g. from `core/intake/evidence-vault`: the `core/intake`, `core`, and project-root markers |
| `{interfaces}` | the contract marker files for every interface in the node's `uses:` + `provides:` |
| `{codepath}` | the node's directory, absolute — use to scope a glob to the work item's own tree |
| `{reqs}` | the directory holding the global `bizreq.iter.md`/`techreq.iter.md` (the `global_bizreq_path` / `global_techreq_path` settings, default `{codepath}/reqs/`), resolved by the engine and exported to agents as `$ITER_REQS`. Exactly those two files are auto-surfaced in every work item's spin-up (as `$ITER_BIZREQ` / `$ITER_TECHREQ`) — never the whole directory — so `{reqs}` globs are for deliberately attaching MORE of that directory |
| any glob/path | passed through as a normal context entry |

Worked example — *"attach this node's marker, everything above it, and every
bizreq/techreq doc anywhere under the node"*:

```
{marker}
{ancestor_markers}
{codepath}/**/*bizreq.iter.md
{codepath}/**/*techreq.iter.md
```

(The leading `*` also matches prefixed names like `payout.bizreq.iter.md`; without
the `{codepath}/` anchor a glob searches from the project root instead.)

## Data mapping

Everything renders from existing engine files — no schema change required except one:

| UI element | Source |
|---|---|
| Summary counts, rows | `.iter/.engine/workitems.jsonl` + `workitems_closed.jsonl` |
| Engine state chip | `stop.signal` presence/content + engine liveness |
| Prework/postwork pill pool | `.iter/prepostwork/*` filenames (minus extension) |
| Agent type selector | `.iter/agents/*.md` basenames |
| Test results badge | **new engine behavior**: on close-out, the engine parses the item's `testgroup.iter.md` blocks and stamps a `tests` summary onto the workitem (`{"passed":98,"total":98}`); UI shows `T:98/98 – 100%`. Until then: blank badge (`T:–`) |
| Time records | `times` object |
| Logs lightbox | engine log file filtered by worker tag |

## Serving & URLs (locked 2026-08-11)

Multiple engines run concurrently — different project directories, different PIDs —
and must neither collide nor blur together.

**Ports — deterministic auto-assignment.** `serve.port: "auto"` (default, in
config.json) hashes the project's absolute path into a range (default 9700–9899);
on the rare clash with an unrelated process, probe upward to the next free port.
Same project → same port every restart, with zero coordination — which keeps insert
targets deterministic for agents, prework scripts, and cron
(`http://pdy-dev.localhost:9741/api/workitems`). Explicit `serve.port: 9779`
overrides for fixed well-known ports. *(Hostnames don't prevent collisions —
`*.localhost` names all resolve to 127.0.0.1; the port is what the OS arbitrates.)*

**Hostnames — cosmetic, free.** Browsers resolve any `*.localhost` subdomain to
loopback with no OS config, so the `url_slug` gives readable URLs:
`http://pdy-dev.localhost:9741/`. The server also answers plain
`http://localhost:9741/`. Privileged ports (dropping `:9741`) and TLS are both
**skipped by decision** — loopback `localhost`/`*.localhost` are already secure
contexts in browsers, so https buys nothing here.

**Server registry — discovery across engines.** Each `iter serve` writes
`{project_name, url_slug, path, port, pid, started}` to `~/.iterapp/servers.json`
on startup and removes itself on shutdown; readers drop rows whose pid is gone.
Powers `iter servers` (CLI list) and the sidebar's **Running Servers** switcher
(`GET /api/servers`), so every webapp links to every other one even if a hash moved
a port.

**Telling projects apart — four branding surfaces**, ordered by how much they help
with ten open tabs:
1. Browser tab `<title>`: `{url_slug} · IterLoop`.
2. Favicon tinted per project — an SVG dot data-URI colored by slug hash, with the
   slug's initial; color beats text across many tabs.
3. Sidebar: `{project_name}` + `{url_slug}.localhost:{port}` under the wordmark, on
   every page.
4. Projects page root row carries `{project_name}` (never a generic "Project").

## API (merge phase)

Served by the same binary — `iter serve --project <p> [--port 9779]`, or
`iter run --serve` to run engine + web server together. Localhost only by default.
A deterministic local port also gives scripts/agents a stable insert path.

```
GET    /api/state                      engine state + counts
POST   /api/engine                     {action: pause|stop|resume|shutdown} — pause
                                       drains; the webapp outlives a stopped loop and
                                       resume restarts it; shutdown exits the process
GET    /api/workitems?state=…          open + closed items, filterable
POST   /api/workitems                  insert (same validation as `iter add`: warn
                                       on unknown type, 409 at max_open_workitems)
GET    /api/workitems/{id}             one item, full detail
PATCH  /api/workitems/{id}             edit request fields (only todo/paused items)
POST   /api/workitems/{id}/action      {queue|complete|pause|clone|followup|delete}
GET    /api/meta                       agents, prepostwork pool, config
GET    /api/workitems/{id}/tests       parsed testgroups for the item
GET    /api/workitems/{id}/logs        matching engine log lines
GET    /api/events                     SSE stream: queue changes, state transitions
                                       (drives live counts + row updates)
GET    /api/history?days=N             per-day closed-item buckets (dashboard)
GET    /api/config                     engine + global settings
PUT    /api/config                     write config.json (atomic tmp+rename)
GET    /api/markers                    scanned project model (nodes + use-cases)
POST   /api/markers/rescan             re-run marker discovery
GET    /api/projectsettings            .iter/projects.json
PUT    /api/projectsettings            write it back
GET    /api/servers                    live rows from ~/.iterapp/servers.json
                                       (pid-checked) — the Running Servers switcher
```

All mutations go through the same record-lock protocol as `iter add`; the web
server is just another external producer, so engine and UI can't corrupt the queue.

## Build plan

- [x] **W0 — digest mockup, write this spec** (2026-08-11)
- [ ] **W1 — static interactive mockup** (`src/webapp/mockup.html`): single
      self-contained HTML file, ~15 fake work items across all states; working summary
      filters, slide-down, context-aware Actions (mutating in-page state), all four
      lightboxes, New/Edit/Clone/Follow-up form, Collapse All, engine chip. Publish for
      review; iterate to perfection.
- [x] **W1b — remaining pages in the mockup**: client-side router; Dashboard (tiles +
      three chart panels, fake history); Iterloop Settings form; Projects hierarchy
      with fake markers, use-case thread, depth legend, Create-WorkItem-from-node;
      Project Settings form.
- [x] **W2 — freeze the design**: fold review feedback back into this spec; extract the
      final CSS/JS structure the Rust server will embed.
- [x] **W3 — API layer**: `serve` subcommand (axum or tiny-http), endpoints above,
      static page embedded via `include_str!`; `run --serve` runs both. Engine stamps
      the `tests` summary on close-out.
- [x] **W4 — wire the page to the API**: replace fake data with `/api/*` + SSE live
      updates; empty/error states; confirm dialogs on delete.
- [x] **W5 — E2E tests**: API tests with the fake runner (insert via POST during a run,
      action transitions, cap refusal); a scripted browser smoke pass.

## Open questions

1. Terminal `failed` items live in the closed file — should Requeue/Clone reopen them
   (copy back to the open queue with attempts reset)? (Mockup assumes yes for Clone,
   Requeue only for open failed.)
2. Datetime display: engine stores ISO-8601 UTC; show UTC (locked ISO look) or local
   with a toggle? (Mockup: ISO UTC.)
3. Delete semantics: hard-delete from jsonl, or move to closed with a `deleted` marker
   for auditability? (Mockup: hard delete with confirm.)
