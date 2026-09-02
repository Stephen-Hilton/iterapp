~# refresh and start iter 
cd ~/dev/pdy-dev/devops/
rm iter
cp ~/dev/iterapp/target/release/iter ~/dev/pdy-dev/devops/
./iter start



-----


Architecture

1. How does V3 relate to storage_backends.md? That plan (2026-08-28) has the engine talking directly to pluggable storage through a Rust Storage trait — "no sync layer, shared storage IS the sync." 
> V3 is a significant change, decoupling data from engine from UI. 
> iter_data should essentially be an API server / container, capable of running in a serverless environment like AWS lambda
> Ideally we'll layer on an MCP server as well, which is likewise stateless; but that can come later / out of scope for now.
> To make this work, it'll need a seperate data persistence layer; initially I'm aiming for AWS DynamoDB for ease; that's the primary / first usecase
> I'd only recommend we create an interface layer between the API and the data persistence, so later on we could (if we want) move to a containerized NoSQL option
> - for example, if we wanted to run in GCP, or even a local SQLite, the data persistance only needs to map to the interface, not rewrite API/MCP code

2. What is iter_data physically?  
> per #1, yes; DDB for data persistence for this first iteration, with an interface to abstract away data structure specifics for future-extensions. 
> but it's also the API server, not just the tables. 

3. Where does the code live? 
> good question; yes, we are breaking the previous  `all in one ./iter binary` paradigm. We now have 3 distinct components:
> - iter_data: container (api + data)
> - iter_webui: compact webserver / webpage; ideally something that can be run with flags for `--local` or `--server` deployments. 
> - iter_engine: rust binary, basically what we have today (minus the web server / data management stuff).  

4. Does structureV2 survive? 
> you mean project structures, aka *.iter.md files -- yes!  All of that stays
> you make a good point however; how do we get that data into the new iter_webui (since it's no longer sitting next to the repo)
> if you have any good ideas, let me know!  Otherwise, we can shelve this as TBD for now, and focus on getting the engine right.


iter_data details

5. iter3_workitem has no project field. 
> embarassing oversight; I just added to the iter.v3.md doc
Related: how does the engine learn about workitem changes? 
> there are two options; I would like your input on which we choose:
> (a) iter3_versions table:  changes to tables X project are flagged (via timestamp) in the versions table. The intention is to flag low-change, large-volume tables, so reloads only happen when needed.  That said, workitems table is going to be high-change.
> (b) iter3_workitems table: alternatively we re-read all of the active project (sort key) records every tick.  More data per hit, but if changes are happening every tick anyway, it just saves a hop
> I don't know which to use, however:  pdy-dev is a good case study; if you monitor how often changes have happened, compared to the tick time, you can do some math and approximate % time a tick would have resulted in new data, and make a recommendation. 
> (c) some other mechanism:  I'm sure there are other good DDB design patterns out there
> please research and make a recommendation.

6. The engines key is a single object, but multi-engine is the point. 
> another embarassing oversight, fixed in the doc
Bigger question: engine dirs are machine-specific paths
> yes, the engine operator will need to edit (via iter_webui) 
> this means an engine definition is tied to a particular server (or well conformed server, like a container)
> I just updated the doc slightly, to make the {topdir} more explicit, and to add "gitrepo" which is needed
> note, it is expected the terminal running the engine will have git installed, configured, and authenticated to github so no auth is needed during operations
Also, precedence between project-level state and per-engine state when they disagree.
> they are separate things; project "state" is aspirational, engine "state" is actual.
> i.e, project could be "running" but all engines are offline or stopped.  This might actually report as "all engines stopped" or some calculated project state; but once the engines come back online, the project's state is still "running" and so things begin again.
> if you disagree, let me know
7. "childstate": "todo" in the agents example — V3 retires TODO in favor of parked, so is that a stale example
> yes, stale; I just updated
8. Small ones: 
does maxdailycost: 0 mean unlimited or spend-nothing? 
> unlimited; if you recommend another value (say, -1 or anything <0) open to those recommendations.
The tag color {"F54927"} isn't valid JSON — assume "#F54927". 
> yes, my bad; updated now
For the question widget, 
> - `::checkbox` == multi-select
> - `::radiobutton` == single-pick
answer land as a new detail row (key: "answer", next order)?
> I was thinking it would simply overwrite; which is why listed values == defaults.  I get the checkbox / radiobutton label idea doesn't work very well; I really don't care what this json looks like; feel free to configure it however makes sense for the requirements, which I think you understand.  Ultimately the idea is that AI Agents will be filling in this information, so we'll need a local tool (like the current `.iter/agents/_tools.md` with `_index.md` design) to explain to agents how to design a working widget from json, if/when they need it.
> alternatively, we could have an iter_engine tool (`bash iter --question_widget <parms>`) or iter_data MCP call to assist building / validating the correct json.  Again, I'll leave details up to you. 
9. Approval signing: --pvtkey 'abcd123' on the CLI puts the private key in shell history — a key-file path would be safer. 
> that's fine, just call it `./iter --approve '6b7c6a1ff1f4' --pvtkeypath ./some/path/mykey.pem` 
> could we also do Envar?   I would prefer `. ./.env` then just `./iter --approve '6b7c6a1ff1f4'`
And who verifies the signature: the iter_data service at write time, or the engine when it considers the item for dispatch?
> should be iter_data, since it is central

Engine

10. Where does usage% come from? 
> same place it comes from today... when I look at the current pdy-dev iterapp, I see: `acct 5h 30% · 7d 14%`
> that has proven remarkably accurate, and the code is already complete and well tested (in the engine)
And is the active-account pointer per-engine or centralized in iter_data — if two engines serve one project, do they walk the switch/stop ladder together? 
> great point; no, I'd rather NOT have two engines both picking the same account; ideally they'd pick different accounts.
> I added an "account" key to each "engine" so we could track that relationship; since one engine can only use one account
> IF there are two "running" engines, we should try to NOT run them on the same account; this shouldn't be a restriction (that can happen, especially as the account consumed usage starts getting high) but Ideally we'd keep them separate. 
> I don't know, maybe we add a "weight" to each decision point, and weight engines away from the same account, but not restrict them; so at the end, if there is only 1 account with sufficient usage left, they both can use that.
> open to suggestions. 
Related: accounts are defined per-project, but Claude usage is per-account globally — two projects sharing Dev1 would double-draw against one budget; is cross-project account state a concern?
> no; for smaller projects this is even expected. 
> that said; It made me realize, we should really have an `iter3_engine` table, removing per-engine definition from the _project table, and leaving just a link. 
> the reason; otherwise for running many smaller projects, you'd have a lot of engine definition duplication.
> can I let you move that data into it's own table definition, in the requirements?

11. Locks: I'd propose iter_data as the lock authority instead of git. 
> accepted!  sounds good; just make sure the logic works with SQLite as well as DDB (via the data internal interface).
12. Mandatory git pre/postwork: engine-enforced implicitly
> yes

Scope and migration

13. "Include pending/incomplete features" — I'd read that as scope_reservation.md and storage_backends.md (both plan-only). Is Question_state.md built or still pending? Anything else on the list?
> Question_state.md is already built (it's the "question" workitem state) but is being extended to json-built widgets (radiobuttons, combobox, etc.) in v3
> scope_reservation is NOT built, but needs to be; this is intended to solve the "`locks whole tree` workitem never run" issue
> storage_backends is the move to SQLite; I think this was built, but this is now superceded by the new iter_data direction
14. Migration: is a migratev3 data tool in scope for the first build — V2-SQLite → iter3, and what about pdy-dev, which is still on V1?
> let's skip the tool;  I will ask you to manually make that migration, when we've tested and are ready
15. Engine auth to iter_data: users authenticate to the webui via Cognito, but what identity does an engine present — a user's credentials, or its own engine credential/API key in the local .env?
> what is the simplest way to do this, given we don't need an automated way to create users. i.e., I'm fine going in and hand-adding every user.  A iter_webui "users" page would be good, but we don't need all the cognito bonuses (password reset, email verification, etc.). I'm fine literally hand-adding username/password as long as its secure enough to get back some kind of secure token.  If there's an easy JWT we can bake into the iter_webui (with long-lived tokens for engines) that would be fine.
> if that's a lot more work than it sounds, we can also do cognito, it just makes the app less portable (for now).

Happy to also share design opinions (the lock proposal in #11 is the one I feel strongest about), but I'll stop at questions for now.
> please share!



Doc edits (delegated items)


   
store a monotonically increasing integer seq rather than a timestamp
> this is fine, but we also want to pull once every N minutes too (i.e., every 6hrs, or 12hrs, or 24hrs, etc.)
> in case something goes awry with the seq integer.  How do we facilitate that?  
> that's the reason for the timestamp initially; is there another way to add that fallback?
 
for iter3_workitem, make project the DynamoDB partition key and id the sort key
> makes sense.

Auth: bake in JWT — it's genuinely the simple option here...  add role (user/engine/admin) to iter3_webui_user 
> sounds good, make it happen!

Account anti-collision: exclusion with fallback, not weights
> sounds good

Smaller answers

- maxdailycost: make absent/null mean unlimited, and keep 0 meaning "spend nothing"
> great
- structureV2 data into the webui: the engine is the only component with the repo, and it already walks the markers — so have it serialize the node tree (nodes, types, teststate, codepaths, usecase links) to a JSON snapshot, hash it, and push to iter_data only when the hash changes (an iter3_project_structure row; the versions table flags it like anything else). The webui renders from that. Cheap because the scan already exists; no shelving needed unless you'd rather defer.
> sounds good, however, pdy-dev is still in active build right now;  plan and build for the migration, but do NOT execute yet; that will take some coordination. 

- Question widget: 
> good
- Approval env var: yes — ITER_APPROVE_KEYPATH (path, not the key material) in .env, so . ./.env && ./iter --approve '6b7c6a1ff1f4' just works, with --pvtkeypath as the explicit override.
> please add a function to iter engine: `bash ./.iter/iter --genkey "stephen" ` which does 2 things: 
> - creates the correctly structured keyfile for the user Stephen in a predefined location (say, `./.iter/keys/stephen.pem`) 
> - add a `./.iter/.gitignore` file (if missing) and add a line to ignores everything in `keys/` 

Two opinions you didn't ask for directly

Keep iter_webui thin; the state machine lives in iter_data only. 
> 100% agree

Concretely I'd build one cargo workspace with three artifacts 
 — iter_core (shared types + storage interface)
 - iter_data (axum handlers that compile to both a container binary and Lambda via cargo-lambda), 
 - iter_engine — with webui as static assets served by a trivial wrapper.

Carry per-item versioned writes into iter_data from day one. 
> this was awkwardly stated, but I *think* I understand, and sure, sounds like a cheap solution if implemented now; approved

I also saved a short memory noting the V3 kickoff, the spec location, and the pdy-dev tick math, so future sessions start oriented.

Say the word on any of those and I'll fold them into iter.v3.md the same way I did the lock design.
> approved; please:
> - update the iter.v3.md doc with all decisions, then append a "build plan" to the bottom
> - commit and push all changes, so the entire tree is clear
> - make a branch for iter.v3
> go ahead and build the iter.v3.md
> I'm going to bed, so no stopping for questions; build to completion, test, deploy (creds in .env), test again; fix anything broken
> make sure you don't clobber the pdy-dev app, which is also deploying to AWS currently
> once done; 