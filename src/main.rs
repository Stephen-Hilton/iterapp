mod agents;
mod config;
mod context;
mod itersched;
mod limits;
mod locks;
mod logging;
mod markers;
mod registry;
mod runner;
mod runtests;
mod scheduler;
mod server;
mod spend;
mod template;
mod testgroups;
mod testsweep;
mod validate;
mod workitems;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use workitems::{Queue, WorkItem};

#[derive(Parser)]
#[command(name = "iter", version = env!("ITER_VERSION"), about = "iterapp: the iterloop engine and webapp in one executable")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the engine loop AND the webapp server; prints the URL
    Start {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Fixed port (default: deterministic auto-port hashed from the project path)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Start the engine loop only (no webapp)
    Run {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Run a single tick (spawn what's spawnable, wait for it, exit)
        #[arg(long)]
        once: bool,
        /// Keep ticking until no eligible work remains and nothing is running
        #[arg(long)]
        until_idle: bool,
    },
    /// Append a work item to the queue (the external-producer reference path)
    Add {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// JSON file containing the work item
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long = "type")]
        item_type: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        mainwork: Option<String>,
        #[arg(long)]
        codepath: Option<String>,
        /// Gitignore-like pattern excluded from the codepath lock scope (repeatable);
        /// the item must not touch those subtrees, freeing them for parallel items
        #[arg(long = "codepath-ignore")]
        codepath_ignore: Vec<String>,
        #[arg(long)]
        priority: Option<i64>,
        #[arg(long)]
        risk: Option<i64>,
        #[arg(long)]
        source: Option<String>,
        /// Testgroup provenance this item exists to serve (escalated plans carry
        /// the label so the non-convergence guard can count the loop's laps)
        #[arg(long = "source-testgroup")]
        source_testgroup: Option<String>,
        /// This item must not dispatch until the named work item closes complete
        /// (descendants included). A workid or any unambiguous suffix — the
        /// convention is the last 12 chars, what the webapp shows (repeatable)
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        /// Satisfy dependencies on the named items' own completion only,
        /// ignoring the items they created (plan items normally wait for
        /// their descendants too)
        #[arg(long = "depends-on-shallow")]
        depends_on_shallow: bool,
        /// How items created BY this one are born: "review" → children land
        /// todo (human gate per stage), "auto" → queued (fully automated).
        /// Unset inherits the creating parent's mode (user items: review)
        #[arg(long)]
        automation: Option<String>,
    },
    /// Synchronous critical review: runs the `_critic.md` persona as a subprocess
    /// and prints its feedback to stdout — no work items involved
    Critreview {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// File containing the material to review (plan text, change summary, …)
        #[arg(long)]
        file: PathBuf,
        /// Context file the critic should also read (repeatable)
        #[arg(long)]
        context: Vec<PathBuf>,
        /// Retries after a failed critic run (a 1-token probe rules out token
        /// exhaustion first; exhaustion aborts with exit 3 instead of retrying)
        #[arg(long, default_value_t = 1)]
        max_retry: u32,
    },
    /// Deterministic test runner: run a testgroup's shell scripts, log to
    /// <test_dir>/runs/, and update the group's lastrun/result/counts.
    /// Plain runs are neutral (never flag anything); --broken / --fixed are the
    /// claim modes that write the critfail fail-flag when the claim is false.
    Runtests {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Testgroup label (as in the testgroup.iter.md JSONL block)
        #[arg(long)]
        group: String,
        /// Narrow a NEUTRAL run to one test id/script (claims always run the whole group)
        #[arg(long)]
        test: Option<String>,
        /// Claim "the defect is still present" (premise step): a fully green group
        /// means the calling work item is stale — fail-flag written, stop working
        #[arg(long)]
        broken: bool,
        /// Claim "the defect is resolved" (completion gate): any red or error means
        /// the claim is false — fail-flag written, the item cannot close as done
        #[arg(long)]
        fixed: bool,
        /// Wall-clock budget (minutes) for the group's scripts; overrun = killed → error
        #[arg(long, default_value_t = runtests::DEFAULT_GROUP_TIMEOUT_MIN)]
        timeout_min: u64,
    },
    /// Run one deterministic test sweep now. The standing loop is a "Test Loop"
    /// scheduled workitem (created from webapp Settings → Test) whose command
    /// invokes this with the flags below — edit the workitem to tune the loop.
    Testsweep {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Testgroups run concurrently
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Priority of fix items for never/no-longer-green groups (lower = sooner,
        /// P0 most urgent; default work is 5, so sweep items fill idle capacity)
        #[arg(long, default_value_t = 6)]
        priority_red: i64,
        /// Priority of fix items for was-green-now-stale groups
        #[arg(long, default_value_t = 8)]
        priority_green: i64,
        /// A group with a green run newer than this many hours is left alone
        #[arg(long, default_value_t = 24)]
        green_stale_hours: u64,
        /// Wall-clock budget (minutes) per testgroup run; overrun = killed → error
        #[arg(long, default_value_t = runtests::DEFAULT_GROUP_TIMEOUT_MIN)]
        group_timeout_min: u64,
    },
    /// Reject the CALLING work item as invalid work ($ITER_WORKID): the engine
    /// moves it to `todo` at the next turn boundary with the reason recorded, so
    /// a human re-evaluates. No retries are burned and nothing lands in the
    /// completed archive — use when the work is out of scope, unclear, or its
    /// premise no longer holds.
    Reject {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Why the work is rejected, and what would make it acceptable
        #[arg(long)]
        reason: String,
    },
    /// Dump the scanned C4 marker tree as JSON — the same scan the webapp, the
    /// test sweep, and validate share (scan_roots + marker_glob, ~ expanded).
    /// The deterministic way for agents to traverse the hierarchy: each node
    /// carries name, level, dir, declared testgroup/test_dir/bizreq/techreq,
    /// and uses/provides.
    Markers {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Deterministically edit a use-case file's ordered `participants:` list —
    /// the engine-owned write path for usecase↔C4 links. Use-case files are
    /// GLOBAL objects, so this works from any agent regardless of lock scope
    /// (record-level edit, like the testwriter's marker-key registration).
    Usecase {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// The *usecase.iter.md file to edit
        #[arg(long)]
        file: PathBuf,
        /// Participant entry "<step> <object-ref>" (e.g. "2 core/intake") to add;
        /// an existing entry with the same object-ref is replaced (repeatable)
        #[arg(long)]
        add: Vec<String>,
        /// Object-ref (or full entry) to remove (repeatable)
        #[arg(long)]
        remove: Vec<String>,
        /// Print the resulting participants, one per line
        #[arg(long)]
        list: bool,
    },
    /// Test-Loop gate editor: park C4 objects / use cases / interfaces out of
    /// the test sweep (`test_loop:` frontmatter flag) or bring them back —
    /// the engine-owned write path, usable from any agent regardless of lock
    /// scope. Omitting a node carries down its whole subtree; `--include` on a
    /// descendant re-enters it (nearest flag wins). `--block` is the hard park
    /// (outside/vendor setup missing): `--include`/`--omit`/`--clear` REFUSE
    /// to touch a blocked flag (exit 2) — only a human editing the marker
    /// file lifts it.
    Testloop {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Park this object out of the sweep (repeatable). Ref = node key,
        /// name, use-case name, interface id, or declaring-file path suffix
        #[arg(long)]
        omit: Vec<String>,
        /// Re-enter this object (repeatable) — works under an omitted
        /// ancestor; refused under a blocked one
        #[arg(long)]
        include: Vec<String>,
        /// Hard-park: needs outside/vendor setup; agent-proof (repeatable)
        #[arg(long)]
        block: Vec<String>,
        /// Remove the flag so ancestors/default apply again (repeatable)
        #[arg(long)]
        clear: Vec<String>,
        /// Print every object with its own flag and EFFECTIVE state
        #[arg(long)]
        list: bool,
    },
    /// One-time migration to the inverted priority scheme (2026-08-17: lower =
    /// sooner, P0 most urgent, default 5). Rewrites every OPEN item and
    /// scheduled template as newP = 10 - P (clamped 0..=10); the closed archive
    /// is history and stays untouched.
    InvertPriorities {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// One-time maintenance sweep: stub "# Long Description / TBD" into every
    /// structure-node marker missing the section, so a plan item can fill them in
    Stubdesc {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Validate *.iter.md files (role-aware, per the filename rule). Reports
    /// problems; --fix applies the safe mechanical corrections (fence
    /// normalization, junk before the fence, quoting prose values that contain
    /// ": "). Exit 0 = clean (info-only), 1 = warn/error findings remain, 2 = bad invocation.
    Validate {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        /// Check one file instead of every *.iter.md under the scan roots
        #[arg(long)]
        file: Option<PathBuf>,
        /// Apply the safe corrections in place
        #[arg(long)]
        fix: bool,
        /// With --file: print the authoritative empty template for that
        /// file's role (from the filename) instead of validating. The file
        /// need not exist; an existing interface file's `kind:` steers which
        /// skeleton is emitted.
        #[arg(long)]
        template: bool,
    },
    /// Queue summary, active agents, and locks
    Status {
        #[arg(long, default_value = ".")]
        project: PathBuf,
    },
    /// Write stop.signal (--wait drains in-flight work first)
    Stop {
        #[arg(long, default_value = ".")]
        project: PathBuf,
        #[arg(long)]
        wait: bool,
    },
    /// Copy the .iter template into a target project
    Init {
        dest: PathBuf,
        /// Template directory (defaults to $ITERAPP_TEMPLATE, then ./src/.iter)
        #[arg(long)]
        from: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Start { project, port } => cmd_start(project, port),
        Command::Run { project, once, until_idle } => cmd_run(project, once, until_idle),
        Command::Add { project, file, item_type, title, mainwork, codepath, codepath_ignore, priority, risk, source, source_testgroup, depends_on, depends_on_shallow, automation } => {
            cmd_add(project, file, item_type, title, mainwork, codepath, codepath_ignore, priority, risk, source, source_testgroup, depends_on, depends_on_shallow, automation)
        }
        Command::Critreview { project, file, context, max_retry } => {
            cmd_critreview(project, file, context, max_retry)
        }
        Command::Runtests { project, group, test, broken, fixed, timeout_min } => {
            cmd_runtests(project, group, test, broken, fixed, timeout_min)
        }
        Command::Testsweep { project, concurrency, priority_red, priority_green, green_stale_hours, group_timeout_min } => {
            cmd_testsweep(
                project,
                testsweep::SweepOptions { concurrency, priority_red, priority_green, green_stale_hours, group_timeout_min },
            )
        }
        Command::Reject { project, reason } => cmd_reject(project, reason),
        Command::Usecase { project, file, add, remove, list } => cmd_usecase(project, file, add, remove, list),
        Command::Markers { project } => cmd_markers(project),
        Command::Testloop { project, omit, include, block, clear, list } => {
            cmd_testloop(project, omit, include, block, clear, list)
        }
        Command::InvertPriorities { project } => cmd_invert_priorities(project),
        Command::Stubdesc { project } => cmd_stubdesc(project),
        Command::Validate { project, file, fix, template } => cmd_validate(project, file, fix, template),
        Command::Status { project } => cmd_status(project),
        Command::Stop { project, wait } => cmd_stop(project, wait),
        Command::Init { dest, from } => cmd_init(dest, from),
    };
    std::process::exit(code);
}

/// Shared startup: heal .iter/, load config, initialize logging. Returns false on failure.
fn boot(project: &Path) -> bool {
    match template::ensure_project(project) {
        Ok(0) => {}
        Ok(n) => println!("initialized {} missing .iter file(s) in {}", n, project.display()),
        Err(e) => {
            eprintln!("error: cannot initialize .iter in {}: {}", project.display(), e);
            return false;
        }
    }
    let cfg = config::load(project);
    // The global requirement stubs heal at their CONFIGURED paths (settings
    // global_bizreq_path/global_techreq_path), so this runs after config loads.
    match template::ensure_global_reqs(project, &cfg) {
        Ok(0) | Err(_) => {} // creation failure is not fatal — the engine runs without stubs
        Ok(n) => println!("created {} global requirement stub(s)", n),
    }
    let log_file = if cfg.globalsettings.log_default_path.is_empty() {
        None
    } else {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H").to_string();
        let rel = cfg.globalsettings.log_default_path.replace("{YYYYMMDD-hh}", &stamp);
        Some(project.join(rel))
    };
    logging::init(&cfg.globalsettings.log_level, log_file, cfg.globalsettings.log_max_size_mb);
    true
}

fn cmd_run(project: PathBuf, once: bool, until_idle: bool) -> i32 {
    if !boot(&project) {
        return 1;
    }
    match scheduler::run(project, scheduler::RunMode { once, until_idle }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {}", e);
            1
        }
    }
}

fn cmd_start(project: PathBuf, port: Option<u16>) -> i32 {
    if !boot(&project) {
        return 1;
    }
    let project = match project.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: bad project path: {}", e);
            return 1;
        }
    };
    let (listener, port) = match server::bind(&project, port) {
        Ok(bound) => bound,
        Err(e) => {
            eprintln!("error: cannot bind webapp port: {}", e);
            return 1;
        }
    };
    let slug = server::slug(&project);
    let settings = server::project_settings(&project);
    let name = settings["project_name"].as_str().unwrap_or("project").to_string();
    registry::register(&project, &name, &slug, port);
    println!();
    println!("  iter version:    {}", env!("ITER_VERSION"));
    println!("  iterapp webapp:  http://localhost:{}/", port);
    println!("                   http://{}.localhost:{}/", slug, port);
    println!();

    // The engine loop runs as a restartable thread so the webapp can pause/resume it;
    // stopping the loop does NOT kill the webapp. Shut everything down with Ctrl-C or
    // POST /api/engine {"action":"shutdown"}.
    let engine = server::Engine::new(project);
    engine.start_loop();
    server::serve(listener, std::sync::Arc::clone(&engine), port);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_add(
    project: PathBuf,
    file: Option<PathBuf>,
    item_type: Option<String>,
    title: Option<String>,
    mainwork: Option<String>,
    codepath: Option<String>,
    codepath_ignore: Vec<String>,
    priority: Option<i64>,
    risk: Option<i64>,
    source: Option<String>,
    source_testgroup: Option<String>,
    depends_on: Vec<String>,
    depends_on_shallow: bool,
    automation: Option<String>,
) -> i32 {
    let cfg = config::load(&project);
    let mut item: WorkItem = match &file {
        Some(path) => match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|t| {
            serde_json::from_str::<WorkItem>(&t).map_err(|e| e.to_string())
        }) {
            Ok(item) => item,
            Err(e) => {
                eprintln!("error: cannot read work item from {}: {}", path.display(), e);
                return 1;
            }
        },
        None => WorkItem::default(),
    };
    if let Some(v) = item_type {
        item.item_type = v;
    }
    if let Some(v) = title {
        item.title = v;
    }
    if let Some(v) = mainwork {
        item.mainwork = v;
    }
    if let Some(v) = codepath {
        item.codepath = v;
    }
    if !codepath_ignore.is_empty() {
        item.codepath_ignore = codepath_ignore;
    }
    if let Some(v) = priority {
        item.priority = v;
    }
    if let Some(v) = risk {
        item.risk = v;
    }
    if let Some(v) = source {
        item.source = v;
    }
    if let Some(v) = source_testgroup {
        item.source_testgroup = v;
    }
    if !depends_on.is_empty() {
        item.depends_on = depends_on;
    }
    if depends_on_shallow {
        item.depends_on_shallow = true;
    }
    if let Some(v) = automation {
        item.automation = v;
    }
    // A typo'd mode silently meaning "review" is the wrong failure mode.
    let mode = item.automation.trim().to_string();
    if !mode.is_empty() && mode != workitems::AUTOMATION_REVIEW && mode != workitems::AUTOMATION_AUTO {
        eprintln!("error: automation must be \"review\" or \"auto\" (got \"{}\")", mode);
        return 2;
    }

    if item.item_type.is_empty() || item.mainwork.is_empty() {
        eprintln!("error: a work item needs at least --type and --mainwork (or a --file providing them)");
        return 1;
    }
    // Only users schedule (webapp API). Agents create work through this path,
    // so a scheduled state or a schedule spec is refused here.
    if item.state == workitems::STATE_SCHEDULED || !item.sched.is_none() {
        eprintln!("error: scheduled work items are user-created in the webapp; `iter add` (the agents' path) cannot schedule work");
        return 1;
    }
    if item.workid.is_empty() {
        item.workid = uuid::Uuid::new_v4().to_string();
    }
    if item.times.added.is_empty() {
        item.times.added = workitems::now_iso();
    }
    if item.state.is_empty() {
        item.state = workitems::STATE_QUEUED.into();
    }
    // Provenance for transitive dependency satisfaction: an `iter add` run
    // inside an engine work item stamps that item as this one's creator.
    if item.created_by.is_empty() {
        if let Ok(wid) = std::env::var("ITER_WORKID") {
            if !wid.is_empty() {
                item.created_by = wid;
            }
        }
    }

    // Warn at add, enforce at pick.
    let known: Vec<String> = agents::discover(&project).into_iter().map(|a| a.type_name).collect();
    if !known.is_empty() && !known.contains(&item.item_type) {
        eprintln!(
            "warning: type \"{}\" matches no agent in .iter/agents/ (known: {}); it will never be picked until such an agent exists",
            item.item_type,
            known.join(", ")
        );
    }

    let queue = Queue::new(&project, &cfg);

    // Automation mode (features/workitem_automation.md), agent-sourced adds
    // only ($ITER_WORKID present — an engine-run work item created this):
    // an unset mode inherits the creating parent's, then the child's state is
    // DERIVED from the effective mode — review → todo (human gate per stage),
    // auto → queued (fully automated). Prompts do not decide state: a
    // prompt-written todo/queued is overridden (and said so); an explicit
    // `paused` survives. Guards applied below (non-convergence) still win.
    let in_workitem = std::env::var("ITER_WORKID").ok().filter(|w| !w.is_empty());
    if let Some(parent_id) = &in_workitem {
        if item.automation.trim().is_empty() {
            let parent = queue
                .load()
                .into_iter()
                .find(|i| i.workid == *parent_id)
                .or_else(|| queue.load_closed().into_iter().rev().find(|i| i.workid == *parent_id));
            if let Some(p) = parent {
                item.automation = p.automation.trim().to_string();
            }
        }
        let derived = if item.automation.trim() == workitems::AUTOMATION_AUTO {
            workitems::STATE_QUEUED
        } else {
            workitems::STATE_TODO
        };
        if matches!(item.state.as_str(), workitems::STATE_QUEUED | workitems::STATE_TODO)
            && item.state != derived
        {
            eprintln!(
                "note: state \"{}\" overridden to \"{}\" — the automation mode ({}) decides how agent-created items are born",
                item.state,
                derived,
                if item.automation.trim().is_empty() { "review, inherited default" } else { item.automation.trim() }
            );
            item.state = derived.into();
        }
    }

    // Dependency resolution: suffixes → full workids, refusing unknown or
    // ambiguous suffixes and cycles (exit 2 — refuse, never guess).
    if !item.depends_on.is_empty() {
        let open = queue.load();
        let closed = queue.load_closed();
        if let Err(e) = workitems::resolve_depends_on(&mut item, &open, &closed) {
            eprintln!("error: {}", e);
            return 2;
        }
    }

    // Non-convergence guard (2026-08-17): the escalation cycle is fix item →
    // plan → build → tests still red → fix item → plan again. Two full laps are
    // allowed; the THIRD plan born for the same testgroup lands in `todo` with
    // this note instead of running, so a human breaks the loop.
    if item.item_type == "plan" && !item.source_testgroup.is_empty() {
        let prior_plans = queue
            .load()
            .iter()
            .chain(queue.load_closed().iter())
            .filter(|i| i.item_type == "plan" && i.source_testgroup == item.source_testgroup && i.workid != item.workid)
            .count();
        if prior_plans >= 2 && item.state == workitems::STATE_QUEUED {
            item.state = workitems::STATE_TODO.into();
            item.mainwork = format!(
                "NON-CONVERGENCE: this is plan #{} born from testgroup \"{}\" — two full \
                 fix→plan→build laps have not turned it green, so this loop is not converging \
                 on its own. Held in todo for human review: reconsider the approach (the tests? \
                 the requirements? the architecture?) before requeueing.\n\n---\n{}",
                prior_plans + 1,
                item.source_testgroup,
                item.mainwork
            );
            eprintln!(
                "warning: plan #{} for testgroup \"{}\" — non-convergence guard holds it in todo for human review",
                prior_plans + 1,
                item.source_testgroup
            );
        }
    }

    let open = queue.load().len();
    if open >= cfg.engine.max_open_workitems {
        eprintln!(
            "error: queue refused: {} open work items >= max_open_workitems ({})",
            open, cfg.engine.max_open_workitems
        );
        return 2;
    }
    match queue.append(&item) {
        Ok(()) => {
            println!("added {} \"{}\" (type {}, priority {})", item.workid, item.title, item.item_type, item.priority);
            0
        }
        Err(e) => {
            eprintln!("error: cannot append work item: {}", e);
            1
        }
    }
}

/// Exit codes: 0 = review on stdout; 1 = critic failed after retries; 2 = bad
/// invocation; 3 = usage/credit limit hit. On 1 and 3 a fail-flag file keyed by
/// $ITER_WORKID is written, which the engine consumes at the next turn boundary
/// to fail the calling work item DETERMINISTICALLY — a requested review that
/// never happened must never degrade into "proceeded without review". The
/// caller is told to stop, but the item fails even if it doesn't.
///
/// Deliberately NO usage-limit gate before spawning: the caller is already
/// running and this review is how it finishes its work — the engine throttles
/// at agent start (max_agents_at_80/90/95), never mid-flight. The critic's spend
/// is still recorded to the ledger as receipts; recording never throttles.
fn cmd_critreview(project: PathBuf, file: PathBuf, context: Vec<PathBuf>, max_retry: u32) -> i32 {
    // Heal so projects created before _critic.md existed still get the persona.
    let _ = template::ensure_project(&project);
    let critic_path = agents::agents_dir(&project).join("_critic.md");
    let text = match std::fs::read_to_string(&critic_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", critic_path.display(), e);
            return 2;
        }
    };
    if !file.exists() {
        eprintln!("error: material file {} not found", file.display());
        return 2;
    }
    let agent = agents::parse("_critic", &text);

    let mut prompt = format!(
        "{}\n\n## Material to review\nRead and review this file: {}\n",
        agent.body,
        file.display()
    );
    if !context.is_empty() {
        prompt.push_str("\n## Context files (requirements to judge against)\n");
        for c in &context {
            prompt.push_str(&format!("- {}\n", c.display()));
        }
    }
    prompt.push_str("\nRead the material and context files now, then return ONLY the review in the output shape specified above.");

    let attempts = max_retry.saturating_add(1);
    let mut last_err = String::new();
    for attempt in 1..=attempts {
        // Fresh session per attempt; a crashed session id is not worth resuming.
        let mut session = runner::Session::new(agent.clone(), project.clone(), project.clone());
        session.own_group = false; // stay in the caller's process group: an engine kill of the caller takes us too
        match session.run(&runner::Turn { label: "critreview".into(), prompt: prompt.clone() }) {
            Ok(out) if !out.text.trim().is_empty() => {
                spend::record(
                    &project,
                    &spend::SpendEntry {
                        ts: workitems::now_iso(),
                        workid: "critreview".into(),
                        agent: "_critic".into(),
                        turn: format!("attempt-{}", attempt),
                        usd: out.cost_usd,
                        input_tokens: out.input_tokens,
                        output_tokens: out.output_tokens,
                    },
                );
                println!("{}", out.text.trim());
                return 0;
            }
            Ok(_) => last_err = "critic returned empty output".into(),
            Err(e) => last_err = e,
        }
        if spend::is_usage_limit_error(&last_err) {
            // Raw error text goes into the flag so the engine's classifier routes
            // it (window limit → hold until reset; billing → drain).
            write_critfail(&project, &format!("critical review aborted: {}", last_err));
            println!(
                "CRITREVIEW ABORT (usage/credit limit): {}. STOP NOW: this work item is flagged to fail; end your work immediately.",
                last_err
            );
            return 3;
        }
        // The critic crashed for an unclear reason — probe with a ~1-token request
        // to separate "out of tokens" from "transient crash worth retrying".
        if let Err(probe_err) = probe_tokens(&project) {
            write_critfail(
                &project,
                &format!("critical review aborted: critic error \"{}\"; token probe error \"{}\"", last_err, probe_err),
            );
            println!(
                "CRITREVIEW ABORT (token probe failed): critic error was \"{}\"; probe error was \"{}\". STOP NOW: this work item is flagged to fail; end your work immediately.",
                last_err, probe_err
            );
            return 3;
        }
        eprintln!(
            "critreview: attempt {}/{} failed ({}); token probe OK{}",
            attempt,
            attempts,
            last_err,
            if attempt < attempts { ", retrying" } else { "" }
        );
    }
    write_critfail(&project, &format!("critical review failed after {} attempt(s): {}", attempts, last_err));
    println!(
        "CRITREVIEW FAILED after {} attempt(s): {}. STOP NOW: do NOT proceed without the review — this work item is flagged to fail and will retry.",
        attempts, last_err
    );
    1
}

/// Deterministic test runner CLI. Exit codes — neutral runs: 0 green, 1 red,
/// 2 error/bad invocation. Claim runs (--broken / --fixed): 0 claim upheld,
/// 3 claim false (the critfail fail-flag is written; the scheduler fails the
/// calling work item at the turn boundary regardless of what the agent does next).
fn cmd_runtests(project: PathBuf, group: String, test: Option<String>, broken: bool, fixed: bool, timeout_min: u64) -> i32 {
    if broken && fixed {
        eprintln!("error: --broken and --fixed are mutually exclusive claims");
        return 2;
    }
    let claim = broken || fixed;
    if claim && test.is_some() {
        eprintln!("error: claims are group-level; --test narrows only neutral runs");
        return 2;
    }
    let cfg = config::load(&project);
    let code_root = config::code_root(&project, &cfg);
    let (tg_file, _) = match runtests::locate_group(&code_root, &group) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: {}", e);
            return 2;
        }
    };
    let run = match runtests::run_group(&tg_file, &group, test.as_deref(), timeout_min) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", e);
            return 2;
        }
    };
    for t in &run.runs {
        println!(
            "{}  {} ({}) [{}] — {}{}",
            match t.outcome {
                runtests::Outcome::Green => "PASS ",
                runtests::Outcome::Red => "FAIL ",
                runtests::Outcome::Error => "ERROR",
            },
            t.id,
            t.name,
            t.shell,
            format!("{}/{} — log: {}", t.pass, t.total, t.log_path.display()),
            if t.detail.is_empty() { String::new() } else { format!(" — {}", t.detail) },
        );
    }
    let pct = if run.total > 0 { run.pass * 100 / run.total } else { 0 };
    println!(
        "tests {}/{} {}% — testgroup \"{}\" {}{}",
        run.pass,
        run.total,
        pct,
        run.label,
        run.outcome.as_str().to_uppercase(),
        if run.full_run { "" } else { " (filtered run — the group's recorded result is not updated)" }
    );

    if broken {
        return match run.outcome {
            runtests::Outcome::Red => {
                println!("CLAIM UPHELD (--broken): the defect reproduces; proceed with the fix.");
                0
            }
            runtests::Outcome::Green => {
                write_critfail(
                    &project,
                    &format!("stale item: --broken claim failed — testgroup \"{}\" is fully green ({}/{})", group, run.pass, run.total),
                );
                println!(
                    "CLAIM FALSE (--broken): testgroup \"{}\" is fully green — this work item is STALE. \
                     STOP NOW: the item is flagged; touch no code and end your work immediately.",
                    group
                );
                3
            }
            runtests::Outcome::Error => {
                write_critfail(
                    &project,
                    &format!("--broken claim aborted: testgroup \"{}\" has script errors — \"couldn't run\" must not pass for \"defect reproduces\"", group),
                );
                println!(
                    "CLAIM ABORTED (--broken): script error(s) in testgroup \"{}\" — the tests could not run, \
                     which is not the same as the defect reproducing. STOP NOW: the item is flagged.",
                    group
                );
                3
            }
        };
    }
    if fixed {
        return match run.outcome {
            runtests::Outcome::Green => {
                println!("CLAIM UPHELD (--fixed): testgroup \"{}\" is fully green.", group);
                0
            }
            _ => {
                write_critfail(
                    &project,
                    &format!("--fixed claim failed: testgroup \"{}\" is {} ({}/{})", group, run.outcome.as_str(), run.pass, run.total),
                );
                println!(
                    "CLAIM FALSE (--fixed): testgroup \"{}\" is {} — the work is NOT done. \
                     This item is flagged to fail; report what remains and stop.",
                    group,
                    run.outcome.as_str()
                );
                3
            }
        };
    }
    match run.outcome {
        runtests::Outcome::Green => 0,
        runtests::Outcome::Red => 1,
        runtests::Outcome::Error => 2,
    }
}

/// Same scan the webapp's Projects view uses (projects.json scan_roots +
/// marker_glob), then stub the missing Long Description sections.
fn cmd_stubdesc(project: PathBuf) -> i32 {
    let settings = server::project_settings(&project);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md").to_string();
    let roots = server::scan_roots(&project);
    let scan = markers::scan(&project, &roots, &glob);
    let stubbed = markers::stub_long_descriptions(&scan);
    println!(
        "{} node marker(s) scanned; {} stubbed with \"# Long Description / TBD\"",
        scan.nodes.len(),
        stubbed.len()
    );
    for path in &stubbed {
        println!("  {}", path);
    }
    if !stubbed.is_empty() {
        println!("next: create a plan work item targeting markers that contain \"Long Description\" + \"TBD\" to write the real descriptions");
    }
    0
}

/// Validate iter files. Same scan roots as the Projects view; findings print one
/// per line as `SEVERITY code file — message`, fixed ones marked.
fn cmd_validate(project: PathBuf, file: Option<PathBuf>, fix: bool, template: bool) -> i32 {
    if template {
        let Some(f) = file else {
            eprintln!("error: --template needs --file <path> to pick the role from the filename");
            return 2;
        };
        return match validate::template_for(&f) {
            Ok(t) => {
                println!("{}", t);
                0
            }
            Err(e) => {
                eprintln!("error: {}", e);
                2
            }
        };
    }
    let roots = server::scan_roots(&project);
    let report = match validate::run(&roots, file.as_deref(), fix) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {}", e);
            return 2;
        }
    };
    for f in &report.findings {
        println!(
            "{:5} {:24} {}{} — {}",
            format!("{:?}", f.severity).to_uppercase(),
            f.code,
            if f.fixed { "[FIXED] " } else { "" },
            f.file,
            f.message
        );
    }
    let remaining = report.findings.iter().filter(|f| !f.fixed).count();
    println!(
        "validate: {} file(s) checked, {} finding(s){}{}",
        report.files_checked,
        report.findings.len(),
        if report.fixed > 0 { format!(", {} fixed", report.fixed) } else { String::new() },
        if remaining > 0 { format!(", {} remaining", remaining) } else { String::new() }
    );
    // Info-level findings (e.g. "deliberately untested") never fail the command.
    match report.worst() {
        Some(validate::Severity::Error) | Some(validate::Severity::Warn) => 1,
        _ => 0,
    }
}

fn cmd_testsweep(project: PathBuf, opts: testsweep::SweepOptions) -> i32 {
    if !boot(&project) {
        return 1;
    }
    let cfg = config::load(&project);
    let report = testsweep::sweep(&project, &cfg, &opts);
    println!("{}", report.summary());
    for note in &report.notes {
        println!("  note: {}", note);
    }
    0
}

/// Reject-flag writer (`iter reject`): same file-flag pattern as critfail, but
/// the engine's response is "back to todo for human re-evaluation" instead of
/// "failed". Requires ITER_WORKID — outside an engine run there is no item.
fn cmd_reject(project: PathBuf, reason: String) -> i32 {
    let workid = std::env::var("ITER_WORKID").unwrap_or_default();
    if workid.is_empty() {
        eprintln!("error: ITER_WORKID is not set — iter reject only works inside an engine-run work item");
        return 2;
    }
    let reason = reason.trim();
    if reason.is_empty() {
        eprintln!("error: --reason must explain why the work is rejected and what would fix it");
        return 2;
    }
    let path = scheduler::reject_path(&project, &workid);
    match std::fs::write(&path, reason) {
        Ok(()) => {
            println!(
                "work item {} flagged for rejection; the engine moves it to todo at this turn's boundary. \
                 Summarize the rejection in your output and finish the turn — do no further work.",
                workid
            );
            0
        }
        Err(e) => {
            eprintln!("error: cannot write reject flag {}: {}", path.display(), e);
            1
        }
    }
}

/// Record-level `participants:` editor for use-case files. Reads the existing
/// entries through the same frontmatter parser the scan uses, applies removes
/// then adds (an add replaces an entry with the same object-ref), sorts by the
/// leading step number, and rewrites only the participants block — the rest of
/// the frontmatter and the narrative body pass through verbatim.
fn cmd_usecase(project: PathBuf, file: PathBuf, add: Vec<String>, remove: Vec<String>, list: bool) -> i32 {
    let path = if file.is_absolute() { file } else { project.join(file) };
    let filename = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if markers::role_of(&filename) != Some(markers::Role::Usecase) {
        eprintln!("error: {} is not a *usecase.iter.md file (the filename declares the role)", path.display());
        return 2;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read {}: {}", path.display(), e);
            return 2;
        }
    };
    let (_, mut participants, _) = markers::frontmatter(&content);
    // The object-ref is everything after the leading step token.
    let ref_part = |s: &str| s.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
    let step_of = |s: &str| s.split_whitespace().next().and_then(|t| t.parse::<f64>().ok()).unwrap_or(f64::MAX);
    for r in &remove {
        let r = r.trim();
        participants.retain(|p| p != r && ref_part(p) != r);
    }
    for a in &add {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        let new_ref = ref_part(a);
        if !new_ref.is_empty() {
            if let Some(existing) = participants.iter_mut().find(|p| ref_part(p) == new_ref) {
                *existing = a.to_string();
                continue;
            }
        }
        participants.push(a.to_string());
    }
    participants.sort_by(|x, y| step_of(x).partial_cmp(&step_of(y)).unwrap_or(std::cmp::Ordering::Equal));

    if !add.is_empty() || !remove.is_empty() {
        // Rewrite: keep every frontmatter line except the old participants block,
        // append the new block just before the closing fence.
        let trimmed = content.trim_start();
        let Some(rest) = trimmed.strip_prefix("---") else {
            eprintln!("error: {} has no frontmatter fence", path.display());
            return 2;
        };
        let Some(end) = rest.find("\n---") else {
            eprintln!("error: {} has no closing frontmatter fence", path.display());
            return 2;
        };
        let mut kept: Vec<String> = Vec::new();
        let mut in_part = false;
        for line in rest[..end].lines() {
            let t = line.trim();
            if in_part && t.starts_with("- ") {
                continue;
            }
            in_part = false;
            if t == "participants:" {
                in_part = true;
                continue;
            }
            kept.push(line.to_string());
        }
        if !participants.is_empty() {
            kept.push("participants:".to_string());
            for p in &participants {
                kept.push(format!("  - {}", p));
            }
        }
        let new_content = format!("---{}\n---{}", kept.join("\n"), &rest[end + 4..]);
        if let Err(e) = std::fs::write(&path, new_content) {
            eprintln!("error: cannot write {}: {}", path.display(), e);
            return 1;
        }
        println!("{}: participants updated ({} entr{})", path.display(), participants.len(), if participants.len() == 1 { "y" } else { "ies" });
    }
    if list {
        for p in &participants {
            println!("{}", p);
        }
    }
    if add.is_empty() && remove.is_empty() && !list {
        eprintln!("nothing to do: pass --add, --remove, and/or --list");
        return 2;
    }
    0
}

/// Test-Loop gate editor (features/test_loop_flag.md). Each edit re-scans so
/// later refs in the same invocation see earlier edits; any refusal (unknown/
/// ambiguous ref, or touching a blocked flag) aborts with exit 2 — partial
/// application is reported, never silent.
fn cmd_testloop(
    project: PathBuf,
    omit: Vec<String>,
    include: Vec<String>,
    block: Vec<String>,
    clear: Vec<String>,
    list: bool,
) -> i32 {
    let settings = server::project_settings(&project);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md").to_string();
    let roots = server::scan_roots(&project);
    let scan_now = || markers::scan(&project, &roots, &glob);

    let edits: Vec<(markers::TestLoopAction, &Vec<String>)> = vec![
        (markers::TestLoopAction::Omit, &omit),
        (markers::TestLoopAction::Include, &include),
        (markers::TestLoopAction::Block, &block),
        (markers::TestLoopAction::Clear, &clear),
    ];
    let mut edited = 0usize;
    for (action, refs) in edits {
        for target in refs {
            match markers::testloop_apply(&scan_now(), target, action) {
                Ok(summary) => {
                    println!("{}", summary);
                    edited += 1;
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    if edited > 0 {
                        eprintln!("note: {} earlier edit(s) in this invocation were already applied", edited);
                    }
                    return 2;
                }
            }
        }
    }

    if list || edited == 0 {
        let scan = scan_now();
        println!("test loop state (flag → effective):");
        for n in &scan.nodes {
            let flag = if n.test_loop.trim().is_empty() { "-".to_string() } else { n.test_loop.trim().to_string() };
            let eff = match markers::effective_test_loop(n, &scan.nodes) {
                markers::TestLoopState::Included => "included".to_string(),
                markers::TestLoopState::Omitted { value, by } => format!("OMITTED ({} via {})", value, by),
            };
            let key = if n.key.is_empty() { ".".to_string() } else { n.key.clone() };
            println!("  object    {:40} {:10} {:8} → {}", key, n.name, flag, eff);
        }
        for u in &scan.usecases {
            let flag = if u.test_loop.trim().is_empty() { "-".to_string() } else { u.test_loop.trim().to_string() };
            let eff = match markers::own_test_loop(&u.test_loop, &u.file) {
                markers::TestLoopState::Included => "included".to_string(),
                markers::TestLoopState::Omitted { value, .. } => format!("OMITTED ({})", value),
            };
            println!("  usecase   {:51} {:8} → {}", u.name, flag, eff);
        }
        for i in &scan.interfaces {
            let flag = if i.test_loop.trim().is_empty() { "-".to_string() } else { i.test_loop.trim().to_string() };
            let eff = match markers::own_test_loop(&i.test_loop, &i.file) {
                markers::TestLoopState::Included => "included".to_string(),
                markers::TestLoopState::Omitted { value, .. } => format!("OMITTED ({})", value),
            };
            println!("  interface {:51} {:8} → {}", i.id, flag, eff);
        }
    }
    0
}

/// The C4 tree as JSON, for agents: the same scan (roots + glob + frontmatter)
/// the webapp/sweep/validate share, printed instead of served.
fn cmd_markers(project: PathBuf) -> i32 {
    let settings = server::project_settings(&project);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md").to_string();
    let roots = server::scan_roots(&project);
    let scan = markers::scan(&project, &roots, &glob);
    match serde_json::to_string_pretty(&scan) {
        Ok(json) => {
            println!("{}", json);
            0
        }
        Err(e) => {
            eprintln!("error: cannot serialize marker scan: {}", e);
            1
        }
    }
}

/// One-time queue migration for the 2026-08-17 priority inversion: open items
/// (scheduled templates included) get newP = 10 - P under the record lock.
fn cmd_invert_priorities(project: PathBuf) -> i32 {
    let cfg = config::load(&project);
    let queue = workitems::Queue::new(&project, &cfg);
    let changed = queue.with_lock(|items| {
        let mut n = 0usize;
        for it in items.iter_mut() {
            let inverted = (10 - it.priority).clamp(0, 10);
            if inverted != it.priority {
                it.priority = inverted;
                n += 1;
            }
        }
        n
    });
    match changed {
        Ok(n) => {
            println!("inverted priority on {} open item(s) (newP = 10 - P); closed archive untouched", n);
            0
        }
        Err(e) => {
            eprintln!("error: cannot migrate priorities: {}", e);
            1
        }
    }
}

/// Flag the calling work item ($ITER_WORKID, injected by the engine) as failed.
/// The scheduler consumes the flag at the next turn boundary. Manual CLI use
/// (no ITER_WORKID) skips the flag — there is no item to fail.
fn write_critfail(project: &Path, reason: &str) {
    let Ok(workid) = std::env::var("ITER_WORKID") else { return };
    if workid.is_empty() {
        return;
    }
    let path = scheduler::critfail_path(project, &workid);
    if let Err(e) = std::fs::write(&path, reason) {
        eprintln!("critreview: cannot write fail-flag {}: {}", path.display(), e);
    }
}

/// A minimal one-word haiku turn: succeeds iff the account can still complete
/// requests. Cheap enough to be negligible, decisive enough to tell "critic
/// crashed" apart from "tokens exhausted".
fn probe_tokens(project: &Path) -> Result<(), String> {
    let agent = agents::AgentDef {
        model: "haiku".into(),
        max_work_timeout_sec: 120,
        ..Default::default()
    };
    let mut session = runner::Session::new(agent, project.to_path_buf(), project.to_path_buf());
    session.own_group = false;
    session
        .run(&runner::Turn { label: "probe".into(), prompt: "Reply with the single word: ok".into() })
        .map(|_| ())
}

fn cmd_status(project: PathBuf) -> i32 {
    let cfg = config::load(&project);
    let queue = Queue::new(&project, &cfg);
    let items = queue.load();
    let count = |s: &str| items.iter().filter(|i| i.state == s).count();
    println!("queue: {} open", items.len());
    for state in ["queued", "in-progress", "paused", "failed", "todo"] {
        let n = count(state);
        if n > 0 {
            println!("  {}: {}", state, n);
        }
    }
    let closed_items = queue.load_closed();
    for item in &items {
        // Dependency gate: name the blocker on gated items (last-12, the
        // webapp-header convention), or the reason the gate can never open.
        let dep_note = if item.depends_on.is_empty() {
            String::new()
        } else {
            match workitems::dep_status(item, &items, &closed_items) {
                workitems::DepStatus::Satisfied => String::new(),
                workitems::DepStatus::Blocked(id) => format!(" blocked-by: {}", workitems::short12(&id)),
                workitems::DepStatus::Failed(why) => format!(" dep-failed: {}", why),
            }
        };
        println!(
            "  [{}] {} \"{}\" type={} prio={} source={} attempts={}{}",
            item.state,
            &item.workid.chars().take(8).collect::<String>(),
            item.title,
            item.item_type,
            item.priority,
            item.source,
            item.attempts,
            dep_note
        );
    }
    println!("closed: {} archived", closed_items.len());

    let mut locks_found = Vec::new();
    collect_locks(&project, &mut locks_found);
    if locks_found.is_empty() {
        println!("codepath locks: none");
    } else {
        println!("codepath locks:");
        for path in locks_found {
            let detail = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<locks::CodepathLockInfo>(&t).ok())
                .map(|i| format!("workid {} agent {} until {}", i.workid, i.agent, i.timeout))
                .unwrap_or_else(|| "unreadable".into());
            println!("  {} ({})", path.display(), detail);
        }
    }
    if scheduler::stop_signal_path(&project).exists() {
        println!("stop.signal: PRESENT (engine will not pick new work)");
    }
    0
}

fn collect_locks(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if path.is_dir() {
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            collect_locks(&path, out);
        } else if name == locks::CODEPATH_LOCK_NAME {
            out.push(path);
        }
    }
}

fn cmd_stop(project: PathBuf, wait: bool) -> i32 {
    let path = scheduler::stop_signal_path(&project);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = if wait {
        format!("{} drain requested by iterloop stop --wait\n", workitems::now_iso())
    } else {
        format!("{} requested by iterloop stop\n", workitems::now_iso())
    };
    match std::fs::write(&path, body) {
        Ok(()) => {
            println!("stop.signal written{}", if wait { " (drain)" } else { "" });
            0
        }
        Err(e) => {
            eprintln!("error: cannot write {}: {}", path.display(), e);
            1
        }
    }
}

fn cmd_init(dest: PathBuf, from: Option<PathBuf>) -> i32 {
    // Idempotent: adds whatever is missing, never overwrites what exists.
    // Default source is the template EMBEDDED in this binary (self-contained deploy);
    // --from <dir> (or ITERAPP_TEMPLATE) merges from a directory instead.
    let external = from.or_else(|| std::env::var_os("ITERAPP_TEMPLATE").map(PathBuf::from));
    let result = match &external {
        Some(dir) if dir.is_dir() => merge_dir(dir, &dest.join(".iter")),
        Some(dir) => {
            eprintln!("error: template {} not found", dir.display());
            return 1;
        }
        None => template::ensure_project(&dest),
    };
    match result {
        Ok(n) => {
            let reqs_added = template::ensure_global_reqs(&dest, &config::load(&dest)).unwrap_or(0);
            println!(
                "initialized {} — {} file(s) added{} (existing files untouched)",
                dest.join(".iter").display(),
                n + reqs_added,
                external.as_ref().map(|d| format!(" from {}", d.display())).unwrap_or_default()
            );
            0
        }
        Err(e) => {
            eprintln!("error: init failed: {}", e);
            1
        }
    }
}

/// Copy src into dst recursively, skipping files that already exist in dst.
fn merge_dir(src: &Path, dst: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dst)?;
    let mut count = 0;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            count += merge_dir(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
            count += 1;
        }
    }
    Ok(count)
}
