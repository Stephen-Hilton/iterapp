mod agents;
mod config;
mod context;
mod locks;
mod logging;
mod runner;
mod scheduler;
mod template;
/// Used by unit tests today; the engine itself reads testgroups in v2 scheduling.
#[allow(dead_code)]
mod testgroups;
mod workitems;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use workitems::{Queue, WorkItem};

#[derive(Parser)]
#[command(name = "iterloop", version, about = "iterapp's looping AI-coding engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the engine loop
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
        #[arg(long)]
        priority: Option<i64>,
        #[arg(long)]
        risk: Option<i64>,
        #[arg(long)]
        source: Option<String>,
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
        Command::Run { project, once, until_idle } => cmd_run(project, once, until_idle),
        Command::Add { project, file, item_type, title, mainwork, codepath, priority, risk, source } => {
            cmd_add(project, file, item_type, title, mainwork, codepath, priority, risk, source)
        }
        Command::Status { project } => cmd_status(project),
        Command::Stop { project, wait } => cmd_stop(project, wait),
        Command::Init { dest, from } => cmd_init(dest, from),
    };
    std::process::exit(code);
}

fn cmd_run(project: PathBuf, once: bool, until_idle: bool) -> i32 {
    // Copy-the-binary deployment: heal any missing .iter/ files before starting.
    match template::ensure_project(&project) {
        Ok(0) => {}
        Ok(n) => println!("initialized {} missing .iter file(s) in {}", n, project.display()),
        Err(e) => {
            eprintln!("error: cannot initialize .iter in {}: {}", project.display(), e);
            return 1;
        }
    }
    let cfg = config::load(&project);
    let log_file = if cfg.globalsettings.log_default_path.is_empty() {
        None
    } else {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H").to_string();
        let rel = cfg.globalsettings.log_default_path.replace("{YYYYMMDD-hh}", &stamp);
        Some(project.join(rel))
    };
    logging::init(&cfg.globalsettings.log_level, log_file, cfg.globalsettings.log_max_size_mb);
    match scheduler::run(project, scheduler::RunMode { once, until_idle }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {}", e);
            1
        }
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
