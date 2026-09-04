# Iter V3 Specs

Iter is an iterative agentic development harness built to quickly and safely guide AI agents to completing complex applications. 

It's made up of 3 components:  
- iter_data: centralized data store which organizes what is done, what needs to be done next, who's going to do that work, and how.  
- iter_engine: local terminal application which reqeusts work from iter_data, creates new headless claude code agents, then updates status when done.
- iter_webui: webUI for creating, managing, and monitoring all engines and projects by operationalizing the iter_data content.

Data is the brains, engine the brawns, and UI the looks and accessiblility.  Data must be in a central store, engine must be 1 or more local, UI can be local or central webserver, doesn't matter.  UI is the only element designed to be multi-tenent.

## Glossary and Abbreviations
- WI = workitem
- Workitem States:  
  - in-progress: actively being worked on right now
  - queued: ready for processing (once dependencies are met)
  - question: awaiting human review, answer, or approval 
  - parked: awaiting some future state to be true, but which cannot be resolved at all right now; usually long-term
  - paused: a short-term pause for editing or some other temporary hold; usually short-term
  - failed: ran for multiple attempts, finally failed to run
  - complete: the work is done and closed
  - scheduled: used ONLY for recurring work that happens on a recurring schedule; i.e., daily, weekly, every X minutes, etc. It does not start itself, instead spawns the appropriately structured new workitem to be executed. 

For migration purposes, TODO can fall into Parked.  

## Include current new features
As part of this rework, please include any pending or incomplete src/features/*.md, so we have the latest version.
For example:
- src/features/scope_reservation.md


## TODO:
- ~~figure out codepath across many engines~~ resolved 2026-09-01: central iter3_lock / "reserve" rows (see Lock Management)


---- 

## ITER_DATA

This is the most important component, as it holds ALL data for the entire application (excepting only the iter_engine config file, which contains only enough information to connect to iter_data).
It's called iter_data, but in fact contains both data, and services that provide data.  i.e., it's more than just a dumb data storage layer, it also includes APIs and MCP that provide THE CORRECT DATA, structured for the client's needs. 

### Data Structures
These are the beginnings of the data strucures required; the build agents should feel free to add as needed.  
The existing iter data is a good place to look for existing data attributes. 

- all tables should be prefixed with `iter3_*` to differentiate from older versions of the table structures. 
- all timestamp columns are stored as UTC -- and displayed in the user's specific timezone via adjustment in the UI



---- 


#### iter3_agent
Definition of an agent, including defaults for all settings and core prompt body.  These defaults can be overridden upon assignment to a particular project (in that record, not here).

id: "name" column
project json:
```json
{
    "name": "code", 
    "desc": "short description of the agent, only used for display (not passed as prompt).",
    "max": 4,
    "childstate": "queued",
    "timeoutsec": 3600,
    "model": "opus",
    "flags": "--dangerously-skip-permissions",
    "closegate": {"verify": "haiku", "requires_children": false, "requires_commit": false, "max_bounces": 1},
    "promptbody": "this is the full prompt body defining agent behavior, goals, tools, etc. Note there WILL be additional prompt content and context appended to this text."
}
```
"closegate" (decided 2026-09-03) is the per-agent completion contract the engine enforces before an item may close complete — see Close Gate under ITER_ENGINE.  Every key is overridable per project in iter3_project "agents" (e.g. `"plan": {"closegate": {"requires_children": true}}`).

#### iter3_project
Projects are a unified body of work which all share the same dev standards; exactly what you'd associate with the word Project. 
The operationally important idea: they all share the same standards, and code is typically published together.

Max agents is designed to be evaluated in order: compare the higer of claude's 5hr or 7d % consumed, to the operation listed; if true, that's the max agent count.
the last "else" is if all other tests fail.  This allows a flexible construction of logic gates (not hardcoded).  If the user forgets to put "else" at the bottom (aka there could be holes where NOTHING is true) then the system should just add "else":4 at the bottom. 

The Claude Accounts themselves are authenticated via long-lived tokens:
```
Details that matter for your setup:

  - Precedence: the env var beats the /login credential. In the documented auth order, CLAUDE_CODE_OAUTH_TOKEN ranks above subscription OAuth from /login, so each spawned session uses whatever token you export, ignoring whatever account ~/.claude happens to be logged into. 
    That's the routing behavior you want — no cross-contamination between terminals.
  - Max accounts: claude setup-token explicitly supports Pro, Max, Team, and Enterprise plans, so minting one per Max20 account works.
  - Lifetime: it's a ~1-year token; no auto-refresh, so you re-mint yearly. It can only make model requests (no Remote Control, no claude.ai connectors) — fine for iter's purposes.
  - Watch out: ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN in the environment would rank above your OAuth token and silently redirect billing to API credits, so make sure iter's spawn environment doesn't carry those.

  So the recipe is: run claude setup-token once per account, store the three tokens, and have iter set CLAUDE_CODE_OAUTH_TOKEN per spawned session (or per terminal). No CLAUDE_CONFIG_DIR juggling needed unless you also want per-account settings/history separation.

  References: https://code.claude.com/docs/en/authentication.md (precedence + setup-token) and https://code.claude.com/docs/en/headless.md (bare-mode caveat).
```
The "accounts" key should list out all accounts available, by name and the environment variable name containing the key (not the key itself, for security reasons). The engine will need it's own .env file specifying those names. 

"maxdailycost" semantics (decided 2026-09-01): absent or `null` = unlimited; `0` = spend nothing (a literal kill switch); any positive number = max $ per day.

The "state": "Running" is the central state for the project; i.e., if there are multilpe engines, we need to centralize one "Running" / "Draining" / "Stopped" location for ALL engines.  The "Draining" also needs to carefully monitor all engines for possible disconnections, to make sure all engines are honoring the command. 

Note the project "state" is ASPIRATIONAL (the commanded state), while each engine's "state" (in iter3_engine) is ACTUAL.  A project can be "Running" while every engine is offline; when engines come back, they see "Running" and resume.  The UI may display a computed status derived from both.
Draining monitoring (BUILT 2026-09-02): `GET /api/projects/{name}/status` computes the drain picture centrally — per-engine liveness (last_seen vs 3x ticksec + 5s grace), in-progress counts, `all_drained`, and `not_honoring` (engines that went stale while still holding in-progress work).  Draining also stops schedule firing, not just picks.

Engine definitions live in their own `iter3_engine` table (below); the "engines" list here just links engine names to this project, so one engine serving many small projects is defined once, not duplicated per project.

id: "name" column
project json:
```json
{
    "name": "my project name",
    "desc": "300-ish word project description; this should describe what the project does at the highest level; it is passed into agents as context, every time, on agent startup.  etc...",
    "state": "Running",
    "gitrepo": "https://github.com/paydaay/pdy-dev",
    "maxagents": {">98%": 0,
                  ">95%": 1,
                  ">90%": 2,
                  "else": 4
                }, 
    "maxdailycost": null,
    "agents": { "plan": {"max": 2, "childstate":"queued"},
                "code": {"max": 4, "timeoutsec": 3600, "model": "fable", "flags": ""}, 
                "testwriter": {"max": 2}, 
                "usecase": {}, 
                "future_agent_name": {"optional_key": "optional_default_override"},
                }, 
    "failure": {"maxattempts": 5,
                "first_retry_second": 10,
                "retry_backoff_exponent": 2
                },
    "engines": ["Engine01"],
    "accounts": [{"name": "Dev1", "token_envar": "DEV1_TOKEN", "order": 3, "switch":30, "stop": 60},
                 {"name": "Dev2", "token_envar": "DEV2_TOKEN", "order": 2, "switch":80, "stop": 99},
                 {"name": "Dev3", "token_envar": "DEV3_TOKEN", "order": 1, "switch":80, "stop": 99}]
}
```

#### iter3_engine
One record per engine (a single engine process on a single host / well-conformed container).  Projects link to engines by name via their "engines" list, so an engine serving many small projects is defined exactly once.
Because the same engine host checks out each project's repo at a different local path, the machine-specific "dirs" block is keyed per-project INSIDE the engine record.
- "state" is the engine's ACTUAL state (vs. the project's aspirational state).
- "last_seen" is a heartbeat the engine writes every tick; staleness (suggest > 3x ticksec) marks the engine disconnected in the UI and during project "Draining" verification.
- "account" is the Claude account this engine is currently using (one engine uses exactly one account at a time).  Keeping it central lets multiple running engines see each other's choice and avoid piling onto the same account (see Multiple Accounts, below).
- engine operators edit these records via iter_webui; the engine itself updates "state", "last_seen", and "account" as it runs.

id: "name" column
engine json:
```json
{
    "name": "Engine01",
    "host": "steves-macbook.local",
    "state": "Running",
    "last_seen": "2026-09-01T22:52:53Z",
    "ticksec": 5,
    "account": "Dev3",
    "queuelock": {"retryms":50, "breaksec":60},
    "projects": {
        "my project name": {
            "dirs": {"topdir": "~/dev/pdy-dev/",
                     "iterdir": "{topdir}/.iter/",
                     "tempdir": "{iterdir}/temp/",
                     "iter": "bash {iterdir}/iter",
                     "usecasedir": "{topdir}/usecases/",
                     "reqsdir": "{topdir}/reqs/" }
        }
    }
}
```


#### iter3_workitem
The core table containing workitems to be completed.  There are actually 2 tables, this _workitem table containing summary information found at the higher-level view (aka displayed on the tile or chip), and the _workitem_detail table, containing longer / larger sized detailed data, such as the actual request text, response text, pre/post work, timings, logs, etc.  This split allows for a smaller data-package for higher-level / aggregate view UI elements, typically pulling the entire table at once, then only pulling the bulky workitem request and response data when drilling into the details.  The two tables share the same primary key of workitem UUID. 

partition key: "project" text column  (so "all items for a project" is one DDB Query)
sort key: "id" UUID column
Versioned writes (decided 2026-09-01): every workitem carries a "version" integer, incremented by iter_data on each write.  Writers pass the version they read ("expect_version"); a mismatch is rejected with a conflict so the writer re-reads and retries.  This prevents an engine closing an item from silently clobbering a simultaneous UI edit (and vice versa).

project json:
```json
{
    "id": "01890a5d-ac70-7db8-8b5d-10505a42232f",
    "version": 7,
    "name": "Name / Label to identify this particular work item, less than 150 characters",
    "project": "my project name",
    "state": "queued",
    "agent": "code",
    "priority": 3,
    "lockdirs": ["{topdir}/src/some/folder/my_component/", "{topdir}/src/some/other/component/"],
    "createdby": "plan (agent)",
    "requestedby": "Susy",
    "blockedby": ["184fa9a3-f967-4a98-9d8f-57152e7cbe64"],
    "attempt": 1,
    "gate_bounces": 0,
    "prework": ["git-pull"],
    "postwork": ["git-commit", "git-push"],
    "ts": { "recieve": "2026-08-29T22:52:53Z",
            "start": "2026-08-29T23:03:51Z",
            "complete": "2026-08-29T23:08:22Z"},
    "tags": [ {"text":"arbitrary tag", "color":"#F54927"} ]
}
```


#### iter3_workitem_detail
Table for workitem detail containing larger data fields; called only by the id. To get any other state data, look at the _workitem table.
Each ID will have one detail row of "request" with order: 0, but can have N-number of key/values 
Some agents have built-in subagents, such as a plan agent's critical reviews; they'd have additional detail rows of "reivew" with an incrementing order.
After completion, it should have a "response" row with the final/highest order.
The engine's close gate (below) adds a "verify" row (valuetype json: verdict, open obligations, reason, evidence) each time it holds an item back, and a "question" widget row (with `"gate": "close"`) when it gives up and hands the item to a human.

Value can be text, or could be a widget-json document, to record structured data.  
Valuetype can be text, or json, or int, etc. 
Whatever is useful for wrapping a little bit of structure around the return data.
That said, when submitted back to the LLM agent, this data needs to be given proper context, so the agent understands what it's looking at.


partition key: "id" UUID column
sort key: "order" int column
project json:
```json
{
    "id": "01890a5d-ac70-7db8-8b5d-10505a42232f",
    "order": 0,
    "key": "request",
    "valuetype": "text",
    "value": "typically long request text typed by human or another agent; it can be 1000s of words long."
}
```

Or, for structured data — the question-widget schema (decided 2026-09-01): a typed fields array.  Field types: text, int, checkbox (multi-select), radio (single-pick), combo.  Listed "value"s are defaults; answering OVERWRITES value in place (no separate answer row).  iter_data validates widget json at write time so malformed widgets bounce immediately; an agent-facing doc (the `_tools.md`/`_index.md` pattern) plus an `iter --question-widget` validate helper teach agents to build them.
```json
{
    "id": "01890a5d-ac70-7db8-8b5d-10505a42232f",
    "order": 1,
    "key": "question",
    "valuetype": "json",
    "value": {
        "title": "Which is greater, X or Y?",
        "summary": "Brief summary so the user knows what's being asked in under 10 seconds.",
        "detail": "Long description with whatever additional context might be needed. Can be quite long.",
        "fields": [
            {"key": "choice", "label": "Pick one or more", "type": "checkbox",
             "options": [{"value": "A", "desc": "Description of Option A"},
                         {"value": "B", "desc": "Description of Option B"},
                         {"value": "C", "desc": "Description of Option C"}],
             "value": []},
            {"key": "other", "label": "Other (freeform)", "type": "text", "value": ""},
            {"key": "age", "label": "Your Age", "type": "int", "value": 42}
        ]
    }
}
```

Closed items are immutable, docs append (decided 2026-09-03).  Motivation: a pdy-dev agent could not leave a closeout record on a completed plan item because the V2 API only edits open items, so the record ended up in a conversation instead of on the item.  V3 rules, enforced in iter_data (thin-webui principle):
- "closed" = state complete or failed.  question / parked / paused stay fully editable.
- A closed item's summary row rejects PUT (403), with one exception: a write that changes nothing but "tags" is accepted, so finished work can still be organized (labels like "regressed" or "superseded").  The only way out is `POST /api/projects/{p}/workitems/{id}/reopen` (users-only; the engine role gets 403): state → queued, gate_bounces → 0, lasterror and ts.complete cleared, plus an appended "doc" row "reopened by <user> (was <state>): <reason>".  Consequence, on purpose: downstream items still queued become blocked again.
- Detail rows on a closed item reject PUT (403; in-place writes such as question answers are for open items).  `POST .../details` appends and is accepted on a closed item ONLY for key "doc" — the same key is valid on open items too (a mid-run note), and the UI badges docs whose ts is later than ts.complete as post-close.
- `POST .../details` is the append verb everywhere: iter_data allocates the next order atomically (create-if-absent on the zero-padded order, retried on a lost race), so two appenders never overwrite each other.  The engine's own response / verify / question rows use it.
- Every detail write is provenance-stamped by iter_data: "by" (JWT principal) and "ts" (UTC).  A closeout record is only a record if it says who and when.
- Agents and humans reach it the same way: `iter --doc <id-or-prefix> --text "..."` or `--file notes.md` (or `-` for stdin), which uses the same unique-prefix lookup as `--approve`.

```json
{
    "id": "01890a5d-ac70-7db8-8b5d-10505a42232f",
    "order": 7,
    "key": "doc",
    "valuetype": "text",
    "value": "Closeout: the ten build items are filed as 8c1…, 9f2…; round-3 critique disposition revised.",
    "by": "code-agent-engine01",
    "ts": "2026-09-03T21:14:02Z"
}
```

#### iter3_project_prepostwork
These define any pre/post work that is allowed/required by workitems, defined per-project (with a default set, overridable per project).

id: "projectname" column  (always matches either "a specific project name" or "<default>", then distinct down to one set by "name" with "<default>" as the loser)
project json:
```json
{
    "projectname": "my project name",
    "name": "git-commit",
    "shell": "git commit -m \"prework: $WORKITEM_NAME\" ",
    "timeoutsec": 30,
    "failhalt": true
}
```

These might be inline shell commands like above, or could be calls to the local iter_engine, or curl commands to a central iter_data api. 
It could also be a call to a local .sh file that is specific to that project. 
A starting list (but please grow): git-commit, git-push, git-pull, git-pr

 

#### iter3_webui_user
This holds information about the _webui user, and their settings and authz.  Access to this app in general is determined by their name being in the list at all; users are hand-added (this is not a broadly distributed app).

Authentication (decided 2026-09-01): baked-in JWT, not Cognito.  iter_data holds the auth: `/auth/login` exchanges username+password (argon2id "pwhash") for a signed JWT (~24h for humans); engines get long-lived tokens (~1y, "role": "engine") minted by an admin.  Verification is stateless signature-checking (secret/keypair in iter_data env), so it works identically in a container and on Lambda.  Revocation: a "tokenver" integer stamped into each token — bump it on the user row to invalidate everything outstanding for that principal.  "role" is one of user | engine | admin.  Cognito remains a later option if SSO/MFA is ever wanted.

id: "user"
project json:
```json
{
    "user": "Susy",
    "email": "Susy@gmail.com",
    "role": "user",
    "pwhash": "$argon2id$v=19$m=19456,t=2,p=1$...",
    "tokenver": 1,
    "css": "/some/location/iter_skin033.css",
    "pubkey": "<some_ed25519_public_key_for_signature_verification>",
    "settings": {"lastview": "Queued", "other_setting":"value" },
    "authz": {"my project name": "admin", "some other project": "readonly"}
 }
```

The pubkey is specifically for "approve" workitems; they'll ask the user for approval, which must be signed by the private key.
To approve some sensitive change, iter provides a special workitem that only resolves when it receives a signed approval: the workitem id signed by an authorized user, aka `./iter --approve '6b7c6a1ff1f4'`.  The private key is located via (in order): `--pvtkeypath ./some/path/mykey.pem` explicit override, else the `ITER_APPROVE_KEYPATH` env var (set in the user's .env — path only, never key material).
That signed output goes right into a special "approval code" field in the workitem.  Verification happens in iter_data (it is central): if any authorized user's pubkey verifies the workitem's ID signature, the item is queued for work; otherwise iter_data clears the approval code, logs the failure, and leaves it needing approval.

`iter --adduser "stephen"` (decided 2026-09-01) does three things:
- generates an ed25519 keypair and writes the private key to `./.iter/users/stephen.pem` (add-only: refuses if the file exists)
- ensures `./.iter/.gitignore` exists and ignores everything under `users/`
- if an iter_data connection is established, registers the user and public key in iter3_webui_user
There is no reset flow: to "reset", delete stephen.pem, run --adduser again, and have an admin paste the new pubkey into the user record via the iter_webui users page.


#### iter3_webui 
I'm not sure if this is needed, but in case it is: we can use this tablename as a placeholder for any global webui settings that can't reasonably be tucked into the webui_user or _project tables; for example, the `"settings":{}` object could get quiet long, so that we want a set of global default values, which may make sense here rather than in _project.  I'll leave that up to the build agent.


#### iter3_versions
Change-signal table (decided 2026-09-01, backed by the pdy-dev case study: only ~0.8% of 5s ticks ever saw a workitem change — 2.1% in active hours, 4.3% in the busiest hour on record — so polling a tiny row before any full reload saves ~98% of read work).
One row per (project, table) — INCLUDING the workitem table.  Each row carries BOTH:
- "seq": a monotonically increasing integer, bumped atomically by the iter_data API on every write to that table (DDB `UpdateItem ADD`; SQLite increment in the same transaction).  Clients cache the last seq they saw; if unmoved, skip the reload.
- "updated": UTC timestamp of the last write — kept for human debugging and display.

Belt-and-suspenders fallback (in case something ever goes awry with seq): the engine ALSO does an unconditional full reload every `full_refresh_minutes` (engine config, default 360 = 6h), ignoring seq entirely.  The fallback needs no timestamp math — it's simply "every N minutes, reload everything regardless" — which self-heals any missed or corrupted seq bump within one interval.

partition key: "projectname"
sort key: "table"
project json:
```json
{
    "projectname": "my project name",
    "table": "workitem",
    "seq": 48213,
    "updated": "2026-08-29T16:47:00Z"
}
```


#### iter3_project_structure
The structureV2 node tree (nodes, nodetypes, teststate, codepaths, usecase links) rendered for the webui.  The engine is the only component with repo access and already walks the markers, so: it serializes the scan result to a JSON snapshot, hashes it, and pushes to this table only when the hash changes (the versions table flags it like anything else).  The webui renders structure views from here.
NOTE: pdy-dev is in active build — plan and build this migration path, but do NOT execute the pdy-dev migration yet; that takes coordination.

id: "projectname"
project json:
```json
{
    "projectname": "my project name",
    "hash": "sha256-of-snapshot",
    "updated": "2026-08-29T16:47:00Z",
    "snapshot": { "nodes": ["..."], "codepaths": ["..."], "teststates": {"...": "..."} }
}
```


## ITER_ENGINE
This is the actual engine that manages the local claude code harness.  iter_engine always pulls and pushes data to/from iter_data so that _data is always up-to-date (instant for updates, 5 second tick by default on idle downloads).
Each tick, the engine polls the iter3_versions seq rows (one tiny read) and only pulls full tables whose seq moved; every `full_refresh_minutes` (default 360) it reloads everything unconditionally as the seq fallback.  It also writes its heartbeat ("last_seen") to its iter3_engine row each tick.
Usage% (5hr / 7d consumed) is tracked per account from two sources that need nothing but the account's long-lived token (REBUILT 2026-09-04 — the V2 statusline-collector never fires in headless `-p` mode, verified on CLI 2.1.260, so it is gone): (1) **per run** — every spawned claude session runs with `--output-format stream-json`; Claude Code emits a `rate_limit_event` line (unifiedWindows 5h/7d utilization + resetsAt, isUsingOverage) that the engine writes to the account's snapshot file itself; (2) **idle probe** — a direct 1-output-token haiku POST to `/v1/messages` with the token as Bearer, reading the `anthropic-ratelimit-unified-*` response headers (sent on a 429 rejection too, so an exhausted account still reports 100%).  Snapshot file: `$ITER_USAGE_DIR/iter3-usage-<account>.json`, default dir `~/.claude`, V2 collector shape plus `overage`/`status`/`source`; the V2 machine-wide snapshot still feeds the single-account case.  An account on overage (`isUsingOverage`, or a window rejected while requests stay allowed) reads as 100% so nothing routes to it — with Extra Usage enabled an exhausted window does not 429, it bills pay-as-you-go.  An expired window (resets_at in the past) reads as 0%, which is how the engine detects the usage refresh after an all-accounts-stopped hold.  When ALL accounts are at/over their stop%, the engine holds all activity and logs it.

The only data that resides in iter_engine is the `.iter/config.json` file, which holds data on where and how to authenticate to iter_data, and credntials which should be held in a seperate .env file (and .gitignore'd)

Logically, this should work very close to how `~/dev/iter/` and `~/dev/pdy-dev/devops/.iter/` works today.  The main difference; this is becoming a multi-user environment, so we need to isolate engine from data and webUI.  
 
### Lock Management 
DECIDED 2026-09-01: iter_data is the lock authority, replacing both the on-disk `.iter.lock` files and the earlier git-pushed-lock idea.  Since all engines already talk to the central iter_data, locks become rows acquired via atomic conditional write — no race window, no wait-and-repull, no lock-commit churn in the repo.  Git remains for code sync only.

The storage interface must expose an atomic "create if absent or expired" operation implemented per backend (DynamoDB: ConditionExpression on PutItem; SQLite: INSERT inside a transaction) — the lock logic itself is backend-agnostic.

#### iter3_lock
id: "project" column
partition: "path" column  (sort key in DDB)
```json
{
    "project": "my project name",
    "path": "{topdir}/src/some/folder/my_component/",
    "kind": "lock",
    "engine": "Engine01",
    "workid": "01890a5d-ac70-7db8-8b5d-10505a42232f",
    "acquired": "2026-09-01T22:52:53Z",
    "expires": "2026-09-01T23:52:53Z"
}
```

Lock lifecycle at workitem start:
- list the project's lock rows (they are few) and check ancestor/descendant path overlap against the workitem's lockdirs (the existing scopes_overlap logic, now engine-side against central rows)
- if clear: conditionally-put one row per lockdir; if ANY put fails (someone else won the race), release the ones acquired and defer the item
- long-running work extends "expires" periodically (heartbeat); an expired row may be deleted/replaced by any engine, conditioned on the expiry value it read
- release all rows when the workitem closes (success or failure)

Scope reservations (src/features/scope_reservation.md barriers) become rows with "kind": "reserve" — which makes the reservation visible ACROSS engines, resolving that feature's known engine-local limitation and the "codepath across many engines" TODO above.

Git behavior is unchanged in spirit but decoupled from locking: "git-*" prepostwork is engine-enforced and non-optional — the engine WILL pull before work and commit+push after.  Per-item prework/postwork lists are for extras only ("git-new-branch", "git-open-pr", and other future work).  Changes are ALWAYS committed and pushed.

### Workflow:
This should be logically very similar (near identical) to `~/dev/pdy-dev/devops/.iter/` project: 
the iter engine:
- syncs any out-of-date metadata from iter data
- checks max total agents / max agent by type, against what is currently running
- if schedule item is true this tick, create a new workitem per the scheduled workitem's instruction
- if there is >1 slot open: evaluate all workitems currently queued, removing items blocked by incomplete dependencies, or items blocked by existing locks
  - if there is a "locks whole tree" item (aka "lockdirs": "{topdir}/") that is higher priority than anything else:
    place "holds" on all slots to drain all work, so the higher-priority work isn't permanently unable to start
    See the src/features/scope_reservation.md as a starting point; this design is against the old application shape, so adjust as needed
- sorts by priority, by agent type, and starts what it can (plan first, then code, then testwriter, then others)
- move whatever "fits" into in-progress state, and run (same as today)
- loop

### Close Gate (decided 2026-09-03)
Motivation: in pdy-dev a plan item closed "complete" whose own final message said "I'm waiting for the review to finish"; it had written a plan document but filed none of the ten build items in it.  Three minutes later its dependent dispatched into a tree where the prerequisite had closed but built nothing, and the dependent's agent correctly rejected.  Root cause in iter: "the agent process exited 0 after its last turn" was the entire definition of complete.  Nothing engine-side ever asked whether the item delivered.

The gate runs inside the engine's close step, only for agent items whose run returned successfully (exec:shell items keep exit-0 as their contract; failures keep the retry ladder).  It has a deterministic half and an LLM half, and every failure of either half is a **bounce**, never a silent close:

Deterministic checks (free, evidence the engine already has):
- **turn cap**: the worker was spawned with `--output-format json`; a result subtype of `error_max_turns` (or anything but `success`) means the agent was cut off, not finished.
- **open review**: any "review" detail row (valuetype json) with no non-empty "disposition" means a critique was recorded and never acted on.
- **requires_children** (closegate, default false; plan agents should set true): the item must have created at least one workitem whose "createdby" is this item's id.  A plan that filed nothing has not planned.
- **requires_commit** (closegate, default false; code agents should set true): the enforced git postwork must have produced a new commit (HEAD moved).  Code that changed nothing has not coded.

LLM verifier ("verify" in closegate: a claude model alias such as haiku | sonnet | opus, or "" to disable; default haiku):
- one extra headless `claude -p` turn on the verify model, read-only tools (Read, Glob, Grep), bounded by `--max-turns`, billed to the same account as the worker so it shows in the usage% ladder.
- it receives the request text, the worker's final message, and the deterministic evidence (commit + diff stat, children created, review rows, turn count).
- it is asked a narrow question — did the final message claim to finish EVERY obligation in the request, and which are still open — and must answer with one json object: `{"verdict": "complete" | "incomplete" | "unclear", "open": ["..."], "reason": "..."}`.  It judges done-ness, not quality; critique review remains a separate mechanism.  An unparseable answer is "unclear".

Outcomes (state transitions, not comments):
- **complete** → close complete, as before.
- any bounce while `gate_bounces < max_bounces` (default 1) → a "verify" detail row is written, `gate_bounces` increments, and the item goes back to **queued**.  The next run's prompt carries a "Close-gate feedback" section: the verdict, the open obligations, and the previous final message, so the agent continues rather than restarts.
- a bounce at the limit, or an **unclear** verdict → a "verify" row plus a "question" widget row (`"gate": "close"`, radio `action`: continue | accept, text `guidance`), and the item goes to **question**.  Answering re-queues it (the widget rule): `continue` feeds the guidance into the next run's prompt; `accept` makes the engine close the item complete on pick without running an agent (honored only while no newer "response" row exists, so a stale accept cannot auto-close a later requeue).
- the bounce budget is deliberately tiny and mirrors the V2 non-convergence guard: the verifier may not loop an item; after one retry a human decides.

Worker prompt addition: every agent prompt ends with a Close Gate paragraph telling the agent its final message must state what was delivered and list any obligation it did NOT complete (prefixed "NOT DONE:"), and that a verifier compares that message to the request before the item closes.  An honest NOT DONE is a cheap bounce; a persuasive summary that hides one is what the verifier exists to catch.

Because the dependency check is simply `state == complete`, holding the plan item open would also have held its dependent — the gate closes both halves of the incident.

### Dependencies are DEEP; Draining is transitional (built 2026-09-04)
V2's workitem_dependency.md semantics carried into V3 (iter_core::dependency_status, shared by engine and webui): a blocker is satisfied only when it AND every item it created (createdby, transitively) closed complete; `blockedby_shallow: true` opts out (migrated from V2 depends_on_shallow); a failed blocker or descendant never releases the dependent — it simply stays queued underneath the failed item (decided 2026-09-04: no parking), so retrying the failed item (reopen → complete) lets the dependent flow back into the stream with no manual requeue.  The webui nests every item under its blocker (or its creator) as deep as the chain goes, 22px per level, in every sort.  Motivation: the pdy-dev tombstone (delete pdy_core_shared) sat first in line with its plan complete but the plan's children still open; the shallow check would have fired it.
Project state control is two-way, Running | Stopped.  Stopping a project with work in progress sets Draining (shown as "Stopping… (N running)", with a force-stop escape); engines call `POST /api/projects/{name}/settle` each tick while Draining and the webui calls it on refresh, and iter_data flips Draining → Stopped once nothing is in progress.  A claim that answers 409 is now logged every tick (silent conflicts hid the migrated-row version bug).

### V2 prompt parity: context assembly, agent tooling, the agent-facing `iter` CLI (built 2026-09-04)
Stephen's ruling: V3 must give agents everything V2 did — the engine has the project and controls the agent.  Built:
- **iter3_agent_tooling** (new table; `/api/tooling`, admin writes): the text V2 kept beside the agents, now central so every engine assembles the same prompt.  kind = `shared` (V2 `_shared.md`, appended to every prompt), `capability` (V2 `_capability/*.md`: indexed in every prompt as name + one-line desc; the agent reads the full text with `iter capability <name>`, skill-style), `source` (V2 `.iter/source/{user,agent,error}.md`, picked by requester), `prepost` (V2 prose pre/postwork steps, each run as its own turn when a work item lists it), `critic` (V2 `_critic.md`, the `iter critreview` persona with its model/flags/timeout).  Migrated with `--migrate-scope tooling`; agents' promptbody no longer carries `_shared` folded in.  Webui: "Agent Tooling" section under Agents (gear to edit/delete, [+] to add; admin only).
- **Prompt assembly** (iter_engine/src/prompt.rs, order load-bearing for the prompt cache): agent promptbody → shared rules → capability index → close-gate paragraph → "# Project context" (main.iter.md + its `globalcontextfiles`, resolved) → "# Source instructions" → "# Work item" (title, id, codepath, priority) → "# Previous attempt" (lasterror + tail of the last response) → "# Context files" (the item's `context` patterns or the project's `default_context`; `{marker}` = the *.iter.md files of the nearest directory at/above the codepath, `{ancestor_markers}` = the same for every ancestor up to and including topdir, `{topdir}` `{codepath}` and globs).  Like V2, files are LISTED for the agent to read, never inlined.  Project gained `mainfile` (default `{topdir}/main.iter.md`) and `default_context`; WorkItem gained `context` and `model` (per-item override).
- **Turn sequence in one session** (`--resume`): prose prework steps → mainwork (the request, preceded by an answered question when the item came back from `question` — the answer outranks the request; the widget row is marked `surfaced` once shown) → prose postwork → self-check ("confirm every instruction was completed; NOT DONE: lines").  A cut-off turn ends the run and the close gate holds the item.
- **`iter` for agents**: the engine writes `{topdir}/.iter/bin/iter` (a shim to `iter_engine cli`) and puts it on PATH with ITER_BIN, ITER_PROJECT (name), ITER_WORKID, ITER_AGENT, ITER_TOPDIR, ITER_MAINFILE, ITER_CONTEXT_FILES, ITER_ITEM_CONTEXT_FILES, ITER_TEST_DIR, ITER_INTERFACE_DIR, ITER_USECASE_DIR, ITER_DATA_URL, ITER_ENGINE_TOKEN, BASH_MAX_TIMEOUT_MS.  Verbs: `add` (V2 flags and --file with V2 or V3 field names; child of the caller, requestedby `agent:<type>`, birth state from the agent's childstate, ids resolve by unique suffix, an item may not depend on its creator), `ask` (question widget + state question; the engine keeps that state at close and skips the gate), `reject` (parked + reason), `doc`, `critreview` (runs the critic persona, records a "review" row, `--disposition` closes the round — what the close gate's open-review check consumes), `capability`, `status`.  Local-file verbs (`runtests`, `validate`, `markers`, `teststate`, `usecase`, `resolve`, `orphans`) are delegated to the V2 binary when `{topdir}/devops/iter` exists (ITER_V2_BIN / ITER_V2_PROJECT) until V3 re-implements them — the agent's command line is unchanged either way.
- **Engines self-register** on first start (record with host + empty projects; assign projects via the engine gear).  Webui: [+] beside Servers & Engines and Agents, Delete agent (confirmed, irreversible), compact agent rows.
- **Nesting rule** (Stephen 2026-09-04): an item nests under what it is actually WAITING ON (the deep gate's answer) as deep as the chain goes; an unblocked, running or closed item is a root; never under its creator.  Run order = runnable items plus everything they are transitively waiting on.

### V2 → V3 gap audit (2026-09-04, overnight)
Compared V2's features (src/features/*.md, the `iter` verbs, the webapp spec) with V3.  Closed in this pass, none disturbing existing behavior:
- **Stop mid-run** (workitem_stop.md): Actions → "Stop (halt mid-run)…" sets `stop_requested`; the engine that runs the item kills the session's process group within a tick, parks the item with `STOPPED by user mid-run`, clears the flag.  Never retried.
- **Retry backoff** (V2 retry_backoff_sec): a failed attempt sets `retry_after` = now + first_retry_second × exponent^(attempt−1); dispatch skips it until then; cleared on claim.
- **Spend recording + maxdailycost**: every run's cost (claude's total_cost_usd + token counts, summed over the session's turns) lands as a "spend" detail row on the item and in a per-project per-day row (`GET/POST /api/projects/{name}/spend`, table iter3_spend).  The engine enforces maxdailycost: 0 picks nothing, >0 stops picking once today's total reaches it (announced once per day), null unlimited.  The header shows "today $x / $cap".
- **Idle usage probe** (V2 limits.probe, REBUILT 2026-09-04): engine `probe_stale_min` (default 20): when nothing is running, EVERY configured account whose snapshot is older than that gets one direct 1-token probe (~9 tokens, no claude process; `ITER_USAGE_PROBE_URL` overrides the endpoint for e2e) — all accounts, because the ladder needs every account's number to switch.  With no accounts configured (ambient CLI login) the haiku nudge is used instead, since only the CLI can reach that login.  `iter_engine --probe` runs the same probe from the shell for every set token.
- **Account rotation verified end to end** on SampleV3 with two accounts by env-var name and fake 1-year tokens: A over switch% → B's token is what the session receives and the engine reports B; both over switch% → lowest order under stop%; all at stop% → hold.  Long-lived tokens plug in exactly as the spec says: `.env` on the engine host holds `<NAME>_TOKEN=…`, the project's accounts list names the env var.
Delegated, not re-implemented (the agent's command line is unchanged): `runtests`, `testsweep`, `validate`, `markers`, `teststate`, `usecase`, `resolve`, `orphans` → the V2 binary at `{topdir}/devops/iter`.
Still open, recorded here for the next pass: (1) **Test Loop / testsweep is V2-native** — the V2 binary files its fix items into the V2 sqlite queue, not iter_data, so the paused "Test Loop" schedule will not feed V3 until runtests/testsweep are ported (largest remaining gap); (2) `codepath_ignore` lock exclusions; (3) live updates via SSE (V3 polls every 10 s); (4) the V2 dashboard's spend charts (the data now exists); (5) `iter export`.

### Engine usage readout + connectivity test (built 2026-09-04)
Every heartbeat carries the active account's usage snapshot (`usage`: five_hour_pct, seven_day_pct, resets, is_using_overage, status, source, ts, age) read from the per-account snapshot file, so a run's cost shows on the engine chip on the next tick — the engine bounces what Claude Code reports to it into iter_data for the webui.  The chip's "test" button POSTs `/api/engines/{name}/test`, which stamps `test_requested`; the engine sees it on its next tick, runs `claude -p "."` on haiku with no other context (billed to the active account's token), and answers via heartbeat with `test_result` {ok, ms, subtype, text|error, requested} plus a fresh usage snapshot (the nudge's own rate_limit_event line), clearing the request.  The webui polls the record until the answer for its request lands.

### Explain / ELI5 (built 2026-09-04)
Every work item's detail lightbox carries an **ELI5** button under the request. It POSTs `/api/projects/{p}/workitems/{id}/explain`, which stamps `explain_requested` (ISO ts) on the item — allowed on closed items, no state change. The engine serving the project runs the read-only **`explain` agent** on it on its next tick, **immediately**: not a work item, not queued, outside the maxagents cap (never in `running`, so it neither counts toward the cap nor delays a drain), whatever the project state, and without locks — the session's tool set is fixed to Read/Glob/Grep (`--allowedTools` + `--disallowedTools Bash,Edit,Write,…`; the agent record's flags are ignored). The prompt is the `explain` agent's `promptbody` (the engine's built-in persona when the record is missing or a stub), the whole item (every detail row except spend/v2/earlier explanations, inline), and the files to read: project head + global context files, the codepath's marker chain (own node file, then every ancestor's), and the item's context files. The output is appended as an **`explained`** detail row (webui label "Explained Simply"; like `doc` it may land on closed items), a `spend` row with `agent: explain` is recorded against the project's daily total, and the engine `DELETE`s `/explain` to clear the flag so the button re-arms. The webui tells the user it runs at once but may take 30–60 s (longer for a large item) and polls the details until the row lands. Model/timeout come from the `explain` agent record (default sonnet / 900 s). Goal: a human who cannot follow a question or a parked/closed item gets a plain-language re-explanation without opening a new item or waiting in the queue.

### Run Now (decided 2026-09-04)
An operator override from the Actions menu.  Setting `"run_now": true` on a queued item (or queueing a parked/paused item with it) tells the engine to start that item on its next tick as soon as its dependencies are complete and no lock overlaps — even when the maxagents cap is already full.  The cap is not raised: the running count simply exceeds it until enough work finishes to bring it back under, so nothing else starts in the meantime.  The engine clears the flag when it claims the item, so a retry after failure queues normally.  The webui refuses the action while a logical dependency is open or an approval is pending, and warns that an overlapping lock still has to clear.  Project state is still respected: a Stopped or Draining project starts nothing.

### Queued is the default create-state
When an agent creates a new work item, and doesn't specify otherwise, it should create as "queued" by default.  
TODO is replaced by Parked, and should organically become a rare state, being for future / parked items only.  
The two biggest incomplete states should be "queued" or "question".  

### Multiple Accounts
An enhancement over previous versions: natively managing multilple accounts.

We will rotate between multiple long-lived account tokens. The tokens will be saved in the .env file, local to the engine, and only associated to a name using data in the projects table. 
Anti-collision across engines (decided 2026-09-01, exclusion-with-fallback, no weights): before applying the ladder below, an engine reads the other Running engines' current "account" from iter3_engine and EXCLUDES those accounts from its candidate list; if the exclusion empties the list, it is ignored and accounts are shared.  Deterministic, nothing to tune, and it degrades to sharing exactly when only one account has headroom left.  The chosen account is written to the engine's iter3_engine row so others can see it.

The logic in selecting / using accounts:
- sort the accounts by "order" asc and use the first (lowest) first; if there is a tie, randomly pick one
- use that account until the 5hr or 7d usage% reaches the percentage found in the "switch" field (i.e., 80 = 80%), then switch to the next account in "order" 
- once ALL accounts have been reduced to at-or-below "switch" percent, then:
- sort the accounts by "order" asc and use the first (lowest) first; if there is a tie, randomly pick one
- use that account until the 5hr or 7d usage% reaches the percentage found in the "stop" field (i.e., 99 = 99%), then switch to the next account in "order" 
- once ALL accounts have been reduced to at-or-below "stop" percent, then stop all activity; start monitoring for the LLM usage refresh (next 5hr or 7d reset)

### Multiple Account Helper
Please create a small helper function called `iter --accounts` that attempt to retrieve / print out all account keys to the terminal; this is to help engine operators do their .env setup 

### Other Engine Helpers
- `iter --adduser "name"` — see iter3_webui_user section (keypair + gitignore + registration)
- `iter --approve '<workid>'` — see approval flow in iter3_webui_user section
- `iter --question-widget` — validate a question-widget json before submitting
- `iter --doc <id> --text "..."|--file <path|->` — append a "doc" detail row (the one write allowed on a closed item)


## ITER_WEBUI

PRINCIPLE (decided 2026-09-01): iter_webui is THIN.  The state machine — legal state transitions, schedule-spawn dedup, approval verification, queue-action rules — lives exclusively in iter_data handlers, so webui, engine, and the future MCP layer all get identical behavior for free.  iter_webui is static assets plus a trivial serving wrapper, run with `--local` or `--server` flags; both talk to the same iter_data API, and Cognito-vs-JWT, hosting, and storage details never leak into it.

This is basically the same as current-state, with a few enhancements:
- "Running Servers" no longer mean what they used to; you'll need one (or more) for Engine, Data, and WebUI
  - that said, there should be some place to examine WHERE the webui / data / engine all reside currently
- Settings should have their own home, seperate from the various servers
- we'll need some sort of readout on the available "accounts" as seen in the project table section, as well as which is currently active. 
- remove the "iterloop" and "iterapp" headers; this can just become one top-level list of screens
- workitems should INDENT tiles under their dependencies; i.e., it should be visually clear what item is awaiting which other item via nested tiles
- workitems need the ability to dynamically structure widgets to support: iter3_workitem_detail json types
- workitems header updates:
  - "blocked by: uuid" should be click-able, moving focus to the dependency line
  - clicking the ID should copy the id to clipboard (not open the item's detail)
- add to "Sort" the attribute "Blocks" which sorts by blocking items

BUILT 2026-09-04 (feedback round after the pdy-dev migration): pinned-bar state chips are ADDITIVE like V2 (queued + question shows both; "total" clears); Sort and Refresh live in the pinned bar; "run order" shows ONLY runnable items (in-progress, or queued without a pending approval) plus open items that have a runnable descendant, nested under the shown blocker — closed items never appear in it; tags render as colored pills on the row between the name and the timestamp (color from the tag, else a stable hash palette); the lightbox is ~50% wider; the migration's "v2" detail row is collapsed as reference-only; "+ New workitem" opens an editable form (name, agent, exec shell, priority, state on save, lockdirs, blocked-by, tags, request); every row and the lightbox carry an "Actions ▾" menu with V2 parity in V3 state names — open items: queue / park / pause / move to question / resume schedule / complete / pause & edit / clone / delete; question: answer / queue without answering; scheduled: run now (clone into a queued run) / pause schedule / retire; closed: clone / follow-up (new item blocked by this one) / reopen / append doc / delete from archive; in-progress: clone only (no mid-run stop in V3 yet).


----

## BUILD PLAN (2026-09-01)

Repo layout: one cargo workspace.  The V2 crate stays at the root untouched (pdy-dev still runs it); V3 lives under `iter3/`:
- `iter3/iter_core` — shared types (workitem, project, engine, agent, lock, user, versions, structure) + the widget/question schema + validation.
- `iter3/iter_data` — axum API server; storage interface trait with `sqlite` and `dynamodb` backends; JWT auth; versions/seq bumping; conditional lock ops; versioned workitem writes; serves the webui static assets in `--local` mode.  Handlers are transport-agnostic so cargo-lambda can wrap them later (Lambda deploy is a later phase, not phase 1).
- `iter3/iter_engine` — the engine binary: config.json + .env, tick loop (versions poll, heartbeat, metadata sync, pick, locks, prework/mainwork/postwork, close), account ladder with exclusion, helpers (--accounts, --adduser, --approve, --question-widget).
- `iter3/webui/` — static assets (thin client per the ITER_WEBUI principle).

Phases:
1. **iter_core + iter_data on sqlite** — all iter3_* tables behind the storage trait; REST API + JWT auth + seq bumps + lock conditional ops + versioned writes; admin bootstrap (first-run admin user).  Everything testable locally with zero AWS.
2. **dynamodb backend** — same trait, table auto-create with the `iter3_` prefix (additive only; never touches existing tables e.g. pdy4-*); smoke-tested against the real account (creds from local .env, never committed).
3. **iter_engine** — tick loop against the iter_data API; git enforced pull/commit/push; exec:shell workitems first (agent spawn ports from V2 next); central locks; scope-reservation barrier rows.
4. **webui (thin)** — login + project/engine readout + workitem tiles with states, detail view, question widgets.  Feature-parity growth comes after E2E.
5. **SampleV3/** — sample project wired to a local engine + iter_data; E2E: enqueue → lock → run → close, dependency blocking, lock overlap deferral, seq-driven reload, versioned-write conflict.
6. **Later**: MCP layer on iter_data; webui feature parity with V2 webapp; structure-snapshot push.

BUILT 2026-09-03 — Lambda deploy and the V2→V3 migration:
- **iter_data on Lambda** (`iter3/deploy_lambda.sh`): the same axum router runs under `lambda_http` when `AWS_LAMBDA_RUNTIME_API` is set (backend forced to dynamodb, prefix from `ITER_PREFIX`, JWT secret from `ITER_JWT_SECRET` so local and Lambda mint interchangeable tokens).  The webui is embedded in the binary (`include_str!`) and served at `/`, so the deploy is one arm64 `bootstrap` zip: cargo-lambda build, an IAM role limited to `iter3_*` tables, a public function URL (the app does its own auth).  Function name `iter3_data`, role `iter3_data_lambda`.
- **`iter_data --migrate-v2 <iter.db>`** imports a V2 project straight through the Storage trait (no server needed, works on sqlite or dynamodb): workitems + critiques from the V2 sqlite, agent defs from `.iter/agents/*.md` (frontmatter → max/timeoutsec/model/flags, `_shared.md` appended to every promptbody), the project description from main.iter.md, an engine record (merged into an existing one by project), and the operator user from `ITER_USERNAME`/`ITER_PASSWORD`.  Field map is documented at the top of `iter3/iter_data/src/migrate.rs`; V2 fields with no V3 home (risk, automation, model, context, testfiles, codepath_ignore, git_start_commit, todo_reason, …) survive verbatim in a `"v2"` detail row, critiques become `"review"` rows, a pending question becomes a question widget with an `answer` field, and `output` becomes the trailing `"response"` row.  Idempotent: existing rows are skipped unless `--migrate-overwrite`.
- **pdy-dev migrated 2026-09-03** from `devops/.iter/.engine/iter.db` (1151 items: 31 queued, 15 question, 12 todo→parked, 4 paused, 1088 complete, 1 failed) into the production `iter3_` tables, project name `pdy-dev`, state Stopped.  Priorities copied unchanged (the open queue already used lower-is-sooner).  NOT ported, by design, until the V3 engine reaches prompt parity: the V2 prose prepostwork steps (V3 prepostwork is shell, git is enforced), the `_capability/*.md` docs and structure-marker context assembly, the `spend`/`questions`/`sched_log` history tables.  The V3 engine must not be pointed at pdy-dev until those land.

