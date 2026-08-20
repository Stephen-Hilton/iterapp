# refresh and start iter 
cd ~/dev/pdy-dev/devops/
rm iter
cp ~/dev/iterapp/target/release/iter ~/dev/pdy-dev/devops/
./iter start
 


 
1. "FAIL the job" is a trap as written — rejection should be a completion, not a failure. 
> a "complete" job falls into a really big bucket: all completed work.  This is much more difficult to raise up, likely rejections will be swallowed up and never seen again.
> we could move to "todo" instead, which is a fairly low-volume, high-attention bucket, and logically fits; todo: re-evaluate the usecase rejection
> as for `"Create Follow-up Request" button only appears on items in complete state` that's not a concern if the user can "pause and edit" then resubmit
> if that's problematic, I'd recommend we either add a "fail now" capability, aka skip the retries, then allow the user to shift from "Failed" to "pause and edit"
> I don't want to mark it as Completed, it's too noisy of a channel, and implies "no more work to do" which isn't true.

2. The plan flow duplicates (and slightly contradicts) the TDD flow.
> part of the confusion is: multiple starting points, with multiple states each; similar workflows, but a little bit different.
> let me try this:
>
> - add usecase, which will almost certainly span multiple C4 objects:
>   - identify what C4 objects are logically required, and which are PRESENT, and which are MISSING (using bizreq/techreq)
>   - for each C4 object MISSING:
>     - plan agent: build a plan to close this gap
>     - human review gate: update as needed, then approve
>     - queue N-number of code / testwriters: build code & interfaces, and testgroups & tests (parallel should be OK)
>     - update usecase references
>   - for each C4 object PRESENT:
>     - update usecase references
>
> - TDD testloop, which sweeps thru all C4 objects and try to run all tests in all testgroups:
>   - if MISSING testgroup or tests, is it because the C4 object is 
>     - missing tests only (minor effort): 
>       - queue N-number of testwriters to build testgroups & tests
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
>     - missing code (major effort): 
>       - plan agent: build a plan to close this gap
>       - human review gate: update as needed, then approve
>       - queue N-number of code / testwriters: build code & interfaces, and testgroups & tests (parallel should be OK)
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
>   
>   - if testgroup and tests are PRESENT, but failing successful runs
>     - syntax / bugfix / simple fix only, or obsolete / bad test (minor effort): 
>       - queue N-number of code agents and/or testwriters to fix issues
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
>     - missing code, or major structural defect (major effort): 
>       - plan agent: build a plan to close this gap
>       - human review gate: update as needed, then approve
>       - queue N-number of code / testwriters: build code & interfaces, and testgroups & tests (parallel should be OK)
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
   
3. Who writes the usecase↔C4 links after the build?
> I don't really care, except:
the plan agent writes the participants: entries when it creates the markers — it already knows the full object list and runs before the parallel work starts; 
> it knows what it proposes, not what was actually built; the user may come along and make changes that land in between marker creation and code complete
it may have to be a post-process.  Or, we could have a specific engine CLI / API set for global objects, like tests and usecases and interfaces, so updates can be made outside of codepath (and deterministically). 

 
4. How does the agent traverse the C4 hierarchy?
"Traverse the context/container/component to make sure each object exists" needs a mechanism. The marker scan (roots + glob + frontmatter parsing, ~-expansion included) lives in engine code; an agent would have to reimplement it with shell globbing and get it subtly wrong. Cheap fix: add an iter markers subcommand that dumps the scanned C4 tree (name, level, dir, testgroup, uses/provides) as JSON — the same scan the webapp and sweep already share. The agent prompt then says "run \"$ITER_BIN\" markers --project \"$ITER_PROJECT\" and compare." Related nicety: the usecases directory isn't exported to agent sessions today (agents get ITER_TEST_DIR, ITER_INTERFACE_DIR, etc. but no usecase equivalent) — adding ITER_USECASE_DIR would give the agent its write target the same way the testwriter gets its test dir.
> sounds good


Smaller clarifications

- "P7": I read this as priority 7 in iterapp's higher-is-sooner integer scheme (above default-5 user work). Worth spelling out in the doc, since "P7" reads as low priority in most orgs' P0-P9 convention — the opposite of what you mean here.
> you ask me this EVERY time.  Obviously this is a problem; as part of this effort, please go thru and INVERT ALL PRIORITY VALUES.  AKA `newP = 10 - P#`  so P7 becomes P3, P2 becomes P8, P5 stays P5, etc.
- Exact filenames: the doc says "global bizreq.md / techreq.md" — the real files are bizreq.iter.md / techreq.iter.md in the project-wide reqs dir ($ITER_REQS, default .iter/reqs/), which are already auto-surfaced to every work item. So the invariant check comes free: the agent just reads what's already in its context.
> correct; *bizreq.iter.md and *techreq.iter.md, and *interface.iter.md and *usecase.iter.md and... etc.  
> remember when looking at bizreq and techreq, the agents need to look at both the GLOBAL and C4 object LOCAL bizreq and techreq files
- Lock scope for the builder itself: suggest codepath = the usecases directory, mirroring the testwriter's narrow-scope pattern — it reads anywhere, writes only usecase files, and never blocks code work.
> yes
- Agent name: existing agents are single lowercase words (code, plan, testwriter). usecasebuilder or just usecase would match the convention; underscore would be a first.
> usecase is fine, good
- Typos while I'm in there: "descrete" → discrete, "mTLA" → mTLS, "it's goals" → "its goals", "seperately" → separately, and the garbled sentence on line 15 ("will include some kind of authorization included").

If you give me answers on the four design questions — especially #2 (reuse the TDD plan flow?) and #3 (who links) — I can rewrite the doc into the same locked-decision format as TDD.md and draft the usecase_builder.md agent definition to match.




1. Interfaces are documentation, not enforcement. 
> good catch, and good recommendation; please implement your recommendation

2. Usecases have no test hook, so "user-centric TDD" isn't steering yet. 
> kind of... the requirements are:
> - add usecase, which will almost certainly span multiple C4 objects:
>   - identify what C4 objects are logically required, and which are PRESENT, and which are MISSING (using bizreq/techreq)
>   - for each C4 object MISSING:
>     - plan agent: build a plan to close this gap
>     - human review gate: update as needed, then approve
>     - queue N-number of code / testwriters: build code & interfaces, and testgroups & tests (parallel should be OK)
>     - update usecase references
>   - for each C4 object PRESENT:
>     - update usecase references
>
> So "new usecase" should be working (if not, it's a bug - I haven't tested yet).
> Your point is: an existing usecase doesn't go thru the above workflow, and doesn't have any testgroups, so... there is no TDD yet.
> So we need to add testgroups to usecases, and make sure that the testloop scanning for tests to run, or testgroups missing, also look at usecases.  
> Once we have that, then usecases with empty testgroups /tests should fall into the other existing workflow:
> 
> - TDD testloop, which sweeps thru all C4 objects (including usecases) and try to run all tests in all testgroups:
>   - if MISSING testgroup or tests, is it because the C4 object is 
>     - missing tests only (minor effort): 
>       - queue N-number of testwriters to build testgroups & tests
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
>     - missing code (major effort): 
>       - plan agent: build a plan to close this gap
>       - human review gate: update as needed, then approve
>       - queue N-number of code / testwriters: build code & interfaces, and testgroups & tests (parallel should be OK)
>       - mark this item as complete; new tests will run next cycle (after authoring is complete)
> 
> ...Or am I missing something?

Guards to set before turning it loose
The new loop has more self-feeding paths than before: gap-plan spawns code items, red tests spawn fix items, fix items escalate to plans, plans spawn more items. The existing brakes are per-testgroup dedup, max_open_workitems, max_attempts, and the usage-tier throttle — but max_cost_usd_per_day defaults to 0 (off). Set it before real use; it's the one guard that bounds a surprise. 
> no; the throttle I'll use is the max_agent_count at 80% / 90% / 95% for 5hr/1wk, since I'm on a subscription.  The cost per day means nothing to me. 

I'd also consider one small addition: if a plan item is born from the same source_testgroup twice (fix → plan → build → still red → fix → plan again), the second one should land in todo with a "this loop isn't converging" note instead of running — a cycle-breaker for the escalation path. That's cheap to add and it's the failure mode I'd most expect from real use.
> good idea, although let's land in to-do on the 3rd iteration; allow it to try twice, then todo the 3rd attempt with your non-convergence note. 




