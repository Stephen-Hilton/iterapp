# Feature: durable state in SQLite

Status: BUILT 2026-08-26. Stephen's directed redesign (issue 11 of the pdy-dev
field report), run as its own programme after issues 1–10, 12 and 13 — never
interleaved with them, because interleaving means writing every fix twice.

## Why

Three forces, all measured on pdy-dev:

- **The webapp slowed as the queue grew.** `/api/workitems` parsed both jsonl
  files and shipped every item — open and archived, full `mainwork` and `output`
  text — on every refresh. 515 items and roughly 2 MB by the time this landed,
  growing without bound. A whole-file parse per request is a curve that only
  bends one way.
- **The open/closed SPLIT was itself a bug source.** Archived items lived in a
  different file, so every action verb that searched only the open one answered
  `no such open work item`. That is issue 3 (the UI offered Retry on an archived
  item and every click failed) and issue 8 (delete was never implemented at all,
  making the archive append-only forever) — two bugs with one cause.
- **Valuable data lived in a directory named temp.** Critique documents — the
  record of what a reviewer caught before an item shipped — were written to
  `.iter/temp/` and referenced from nothing durable. Issue 9's TTL sweeper would
  have deleted them on schedule. Stephen's rule for this redesign: **no
  non-ephemeral file may live where a cleanup policy can eat it.**

## Storage

SQLite through `rusqlite` with `features = ["bundled"]`, so SQLite compiles into
the `iter` binary and the tool stays a single executable with no system
dependency. One database per project at `.iter/.engine/iter.db`, in WAL mode
with `busy_timeout` set.

WAL matters because the engine, the webapp and the CLI are three separate
processes on the same data: readers run while a writer commits, and a contended
write waits instead of failing. That is precisely what the hand-rolled `.lock`
sentinel files and their retry/ghost-break timers were emulating, so those
retire.

### `archived` is a column, not a state

The single most important schema decision. A `failed` item is still OPEN while
it waits out its retry backoff; a `failed` item that exhausted `max_attempts` is
archived. Deriving "archived" from the state string would resurrect items
mid-retry, so the flag is explicit and the two stay independent.

### Tables

- **`workitems`** — replaces BOTH `workitems.jsonl` and
  `workitems_closed.jsonl`. Hot fields (`workid`, `state`, `item_type`, `title`,
  `priority`, `source`, `created_by`, `added`, `closed`, `archived`) are real
  indexed columns; the full item rides in `body` as JSON, which SQLite queries
  natively when something rarely-used is needed. `seq` preserves insertion
  order, which the file form gave for free and the picker's tie-breaks rely on.
  Retry, clone and delete are now uniform row operations — the split that
  produced issues 3 and 8 no longer exists to be wrong about.
- **`critiques`** — every critical-review round, written by `iter critreview`
  ITSELF at the moment it runs (the engine-owned write path, the same idiom as
  `iter usecase` and `iter teststate`; agents never handle storage). Carries the
  reviewing persona, the material, the full critique, and a `disposition` the
  consuming agent reports back. The purpose is explicit: after months of
  accumulation, "how many rounds had findings, how many led to revisions, per
  agent type, per month" must be a query, not an archaeology dig through
  whichever temp files happened to survive.
- **`questions`** — question, answer, asked/answered timestamps, asking workid.
  The pair still lives on the work item (that is what the agent reads); the
  table is what makes "how long did each decision wait on a human" answerable.
- **`spend`** and **`sched_log`** — straight ports of the jsonl append streams.

Still files, and still sweepable: work-item drafts an agent feeds to `iter add
--file`, intermediate scratch, anything an agent writes for its own use inside
one item's lifetime. The test: **if losing it after 7 days loses information
someone will want, it was never temp — it goes in a table.**

## The API diet — where the felt speed lives

Swapping storage alone does not fix wait times; the payload does.

- The list view ships **summaries** for open items — enough to render a row and
  its chips, with `mainwork`, `output`, `context`, `prework`/`postwork` and the
  question/answer bodies omitted, each marked `"summary": true`.
- Header counts come from `SELECT state, COUNT(*)`, not from counting the
  shipped array — the list is a window now, so counting it would under-report.
- Full bodies load per item on open, via `GET /api/workitems/<id>`.
- The archive paginates (`?archived=1&offset=&limit=`).

At 5,000 items this stays flat where the old design degrades linearly. It
composes with the event-delta work (issue 13) but does not depend on it.

## Migration and the escape hatch

- **One-time importer**, run on first touch of a project and skipped forever
  after: read the existing jsonl, insert inside one transaction, verify the
  count, and only then retire the files — **renamed to `*.imported`, never
  deleted**, until a clean week has passed. A short count leaves the files
  exactly where they are.
- Verified against a copy of pdy-dev's real queue: 56 open + 459 archived = 515
  rows, counts preserved exactly.
- **`iter export`** dumps any table back to jsonl for backup, grep and
  git-friendly inspection. The one genuine loss in this move is that the queue
  stopped being a text file; `iter export` plus the `sqlite3` CLI are the
  replacement.

## What did NOT change

`Queue`'s API. Every caller in the engine, the CLI and the server was written
against `load` / `with_lock` / `append` / `close` / `mutate`, so `Queue` became
a facade over the database keeping those exact signatures — which is why the
whole existing test suite kept passing through the swap. New capabilities were
added beside them (`get`, `counts`, `closed_page`, `remove`), not in place of
them.
