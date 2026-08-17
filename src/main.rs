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
#[command(name = "iter", version, about = "iterapp: the iterloop engine and webapp in one executable")]
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
    },
    /// Run one deterministic test sweep now (the engine also sweeps on a schedule)
    Testsweep {
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
        Command::Add { project, file, item_type, title, mainwork, codepath, codepath_ignore, priority, risk, source } => {
            cmd_add(project, file, item_type, title, mainwork, codepath, codepath_ignore, priority, risk, source)
        }
        Command::Critreview { project, file, context, max_retry } => {
            cmd_critreview(project, file, context, max_retry)
        }
        Command::Runtests { project, group, test, broken, fixed } => {
            cmd_runtests(project, group, test, broken, fixed)
        }
        Command::Testsweep { project } => cmd_testsweep(project),
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
fn cmd_runtests(project: PathBuf, group: String, test: Option<String>, broken: bool, fixed: bool) -> i32 {
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
    let run = match runtests::run_group(&cfg, &tg_file, &group, test.as_deref()) {
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

fn cmd_testsweep(project: PathBuf) -> i32 {
    if !boot(&project) {
        return 1;
    }
    let cfg = config::load(&project);
    let report = testsweep::sweep(&project, &cfg);
    println!("{}", report.summary());
    for note in &report.notes {
        println!("  note: {}", note);
    }
    0
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
    for item in &items {
        println!(
            "  [{}] {} \"{}\" type={} prio={} source={} attempts={}",
            item.state,
            &item.workid.chars().take(8).collect::<String>(),
            item.title,
            item.item_type,
            item.priority,
            item.source,
            item.attempts
        );
    }
    let closed = std::fs::read_to_string(&queue.closed_path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    println!("closed: {} archived", closed);

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
            println!(
                "initialized {} — {} file(s) added{} (existing files untouched)",
                dest.join(".iter").display(),
                n,
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
