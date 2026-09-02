# storage_backends — one engine, pluggable durable storage, UI anywhere

Plan only (2026-08-28) — nothing here is built. Supersedes the command-inbox
+ status-mirror draft (remote_queue.md, deleted): that design solved a
conflict problem iterapp mostly doesn't have, because the engine never holds
queue state across ticks — it re-reads config and re-loads the whole queue
from storage every tick already. This plan formalizes that.

## Goal

- **One interface** between what the iterloop engine needs and **many
  storage options**: sqlite (local default) or dynamodb (remote). jsonl is
  the migration/interchange format, not a live backend (see below).
- **A data-only migration tool**: move the durable tables between jsonl ⇄
  sqlite ⇄ dynamodb with little pain.
- **The web UI runs anywhere** — served locally by `iter start` as today, or
  hosted on AWS — against the same storage.
- **The engine always stays local** and pulls from whichever storage the one
  `storage` setting names. Agents, code, locks, scanning: unchanged, local.

Payoff: with storage on dynamodb and the UI on AWS, the queue is operable
from any browser (mobile included). The laptop asleep? The UI still files
and edits items in DynamoDB; the engine catches up on wake. No sync layer,
no mirror, no command rows — shared storage IS the sync.

## Why this fits the code as it stands

- `scheduler.rs` re-loads everything per tick (`queue.load()`, config
  re-read) — the engine is already a storage-poller.
- `Queue` + `db.rs` are already the de-facto interface: `load`,
  `load_closed`, `get`, `counts`, `archived_ids`, `closed_page`, the mutate
  path, `record_question/answer/critique/spend/sched`, `spend_for_day`,
  `export_table`. The trait is extracted from here, not invented.
- Durable state just consolidated into five SQLite tables (item 11):
  `workitems`, `questions`, `critiques`, `spend`, `sched_log`. That is the
  exact scope of the trait. Everything else (codepath locks, marker scan,
  logs, temp) is about the local code tree and stays filesystem.

## The trait

```rust
trait Storage {           // sketch — names from db.rs, not final
    // workitems
    fn load_items(&self, archived: bool) -> Vec<WorkItem>;
    fn get_item(&self, workid: &str) -> Option<(WorkItem, bool)>;
    fn put_item(&self, item: &WorkItem, archived: bool, expect_version: Option<u64>) -> Result<(), Conflict>;
    fn delete_item(&self, workid: &str) -> bool;
    fn counts(&self) -> HashMap<String, i64>;
    fn closed_page(&self, offset: i64, limit: i64) -> Vec<WorkItem>;
    // questions / critiques / spend / sched_log: the record_* + read fns as-is
    // change signal
    fn seq(&self) -> u64;  // bumped on every write; replaces db-file mtime
}
```

Backends:

- **sqlite** — the current `db.rs`, near-verbatim. Default; zero-config.
- **dynamodb** — one table per durable table (`ITER_<project>_workitems` …
  or one table keyed by project — question 3), on-demand billing,
  conditional writes for versioning. Config: region + table prefix + the
  standard AWS credential chain.
- **jsonl** — NOT a live backend. Item 11 retired live jsonl, and the
  2026-08-27 audit deleted the record-lock settings that existed only to
  make concurrent jsonl writes safe; a live jsonl backend would resurrect
  that machinery. jsonl lives on as the export/import format the migration
  tool speaks (`iter export --table` already emits it).

## The write path: retire the `with_lock` convention

The storage layout is already right: one row per workitem (PK `workid`)
since item 11 — nothing to redesign there. What has to go is the CALLING
CONVENTION on top of it. `Queue::with_lock` ("the ONE mutation path") loads
ALL open rows into a Vec, lets the caller edit the set, then `replace_open`
writes the whole set back — upserting everything and DELETING any open row
not in the Vec — in one transaction. Even single-field `mutate(workid, …)`
rides it. Its own doc comment names the ancestry: "same contract as the old
load-modify-rewrite under a file lock" — the jsonl convention transplanted
onto SQLite, safe today only because one process's mutex serializes callers.
With a second writer (the AWS-hosted UI's Lambdas), a concurrent insert gets
swept by the delete-what's-missing pass and edits are whole-queue
last-write-wins.

The work is therefore a migration chore, not a design problem: ~12
`mutate()` sites become a per-record read-modify-write; ~15 direct
`with_lock` callers (append, close, archive, the pick loop, dependency
release, drain-requeue, the itersched fire that touches template + clone
together) become per-record ops, or explicit small multi-item transactions
where they genuinely touch several items (SQLite tx; DynamoDB
TransactWriteItems). `with_lock`/`replace_open` are deleted at the end of
phase 1.

One nuance remains: the row stores the item as one JSON `body` blob, so a
per-record write is still a whole-ITEM write — engine closing an item while
a phone bumps its priority is last-write-wins on the blob. Trait semantics:
per-item `version` + conditional write (`Conflict` → re-read → retry);
backends may additionally use field-level partial updates internally
(DynamoDB UpdateExpression / SQLite json_set), the same trick the
hilton.zone workqueue Lambdas use so status writes and edits never clobber
each other.

## Change signaling

Local SSE (`/api/events`) fingerprints db-file mtime — meaningless for
DynamoDB. Replacement: a `seq` counter row bumped by every write, exposed on
the trait. Engine and UI ask "did seq move?" before a full load; the SSE
loop keys off `seq` instead of mtime. This also caps DynamoDB cost: idle
ticks cost one tiny read. (Ballpark at the 200-item queue cap: full loads
every 5s tick ≈ single-digit $/month even without the seq check; with it,
near-zero idle.)

## The web UI anywhere

- **Locally**: unchanged — `iter start` serves app.html + `/api/*`, now
  through the trait.
- **On AWS** (decided 2026-08-28): app.html (static) from S3/CloudFront;
  `/api/*` behind an **API Gateway with a Cognito auth flow**, both
  provisioned by iterapp's own infra (no dependency on another repo's
  shared layer). Handlers are Lambda **running the same Rust handlers**
  (Rust Lambda runtime; server.rs handlers factored transport-agnostic —
  they largely are, taking Req → Resp). One codebase, one state machine
  (queue-action rules, schedule dedup, question answering) — no JS
  reimplementation to drift. The Lambda build links the dynamodb backend
  only.
- **Credentials** (decided 2026-08-28): the local side — engine, migration
  tool, deploy scripts — reads AWS keys from a local `.env`
  (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_DEFAULT_REGION), the
  same convention the hilton.zone local monitor uses. Browser users never
  touch keys — they sign in through Cognito.
- Engine-coupled endpoints (engine start/stop, log tail, marker scan,
  validate) don't exist on AWS — the UI already knows engine-present vs
  absent (LIVE/mock split); an "engine offline, queue still editable" state
  replaces them, driven by an engine heartbeat row (last_seen) the engine
  writes each tick.

## Settings

New `storage` section in local `.iter/.engine/config.json` (Settings UI
group "Storage"):

```json
"storage": {
  "backend": "sqlite",            // sqlite | dynamodb
  "dynamodb_region": "us-west-2",
  "dynamodb_table_prefix": "ITER_pdy_",
  "aws_profile": ""               // empty = default credential chain
}
```

Bootstrap rule: this section must live in the LOCAL file — the storage
location cannot be read from the storage it names. The AWS-hosted UI gets
the same values from Lambda env at deploy time. It renders in the Settings
UI (editable when browsing the local server), with the note that a backend
change does not move data — the migration tool does.

## Migration tool

`iter storage migrate --to dynamodb` (and `--to sqlite`, `--to jsonl` for
export): stream the five tables from the configured source to the target,
verify counts, then print the config line to flip. Mostly assembled from
existing pieces — `export_table` + the `import_*_jsonl` readers + the new
dynamo backend's writer. Refuses to run while the engine is up. Data only:
no settings, no locks, no logs.

## Phasing

1. **Extract the trait** over sqlite; refactor the write path to per-item
   versioned writes; add `seq`. Pure refactor, no AWS — everything must
   behave identically after.
2. **dynamodb backend** + migration tool; engine runs local-with-remote-
   storage. Local webapp still the only UI.
3. **AWS-hosted UI**: Rust handlers on Lambda, app.html on S3, auth,
   heartbeat/engine-offline state. Mobile now works.
4. **Later**: notifications (question asked / spend cap hit), multiple
   engine boxes per project (versioned writes already make this safe-ish;
   codepath locks are per-box and would need thought).

## Open questions (queued for review)

1. **Auth — SETTLED 2026-08-28**: iterapp provisions its own API Gateway +
   Cognito auth flow; local side authenticates with AWS keys in a local
   `.env`. No hilton.zone shared-layer dependency.
2. **Rust-on-Lambda confirmed?** Still open: the API Gateway decision says
   WHERE the handlers run, not what language. Recommendation stands —
   compile the same Rust handlers (cargo-lambda) so the state machine is
   never reimplemented in JS.
3. **DynamoDB layout**: table-per-durable-table with a project prefix
   (`ITER_pdy_workitems`, matches the local model) vs one shared set keyed
   by project (fewer tables, one deploy serves all projects — the
   hilton.zone convention)? Leaning: shared set keyed by project.
4. **Conflict UX**: on version conflict from the UI (item changed under
   you), silently re-apply the field edit onto the fresh row, or surface
   "item changed, review and resave"? Engine side is always retry-merge;
   the question is only the human side.
5. **spend day-sums and critique round counters** do read-modify-write
   aggregation; per-item versioning covers them, but DynamoDB atomic
   counters would be simpler — OK to let the backends differ internally as
   long as the trait semantics match?
6. **jsonl demotion — settled 2026-08-28?** jsonl is migration/export
   format only (inherently local), not a live backend; the `with_lock`
   whole-set convention is its last remnant and phase 1 deletes it. Flagged
   here only in case zero-dependency live inspection ever matters.
7. **Heartbeat cadence + staleness display**: engine writes last_seen each
   tick (5s); what does the AWS UI call "offline" — 30s? 5min? And should
   an offline engine block queue-affecting actions or just badge them?
8. **Cost guardrail**: on-demand DynamoDB at the current tick rate is
   single-digit $/month; fine? Or add a `storage.poll_seconds` knob for the
   engine when idle (contra the 2026-08-27 audit's "no knobs nobody
   tunes")?
9. **Settings sync**: config.json stays local by design; is engine-config
   editing from the AWS UI ever wanted (would need a settings table +
   engine re-read), or is queue-only the permanent scope?
10. **Migration safety**: is "refuse while engine is up" enough, or should
    migrate also snapshot the source (sqlite file copy / dynamo export) as
    an automatic pre-step?
