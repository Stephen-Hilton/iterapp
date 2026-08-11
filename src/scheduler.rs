use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::agents::{self, AgentDef};
use crate::config::{self, Config};
use crate::context;
use crate::locks;
use crate::logging;
use crate::runner::{Session, Turn};
use crate::workitems::{self, Queue, WorkItem};

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Prework,
    Mainwork,
    Postwork,
    SelfCheck,
}

struct StepTurn {
    phase: Phase,
    turn: Turn,
}

pub struct RunMode {
    pub once: bool,
    pub until_idle: bool,
}

struct Shared {
    project_root: PathBuf,
    cfg: Config,
    /// Serializes every load-modify-save sequence on the queue within this process.
    /// (The on-disk record lock protects against OTHER processes.)
    queue_mutex: Mutex<()>,
    /// Immediate-stop flag: workers requeue their item between turns when set.
    stop_now: AtomicBool,
    /// Codepath-conflict backoff: workid → don't re-pick before this instant. Purely
    /// in-memory noise suppression; a restart just retries sooner.
    deferred: Mutex<HashMap<String, Instant>>,
}

impl Shared {
    fn queue(&self) -> Queue {
        Queue::new(&self.project_root, &self.cfg)
    }
}

pub fn stop_signal_path(project_root: &Path) -> PathBuf {
    config::engine_dir(project_root).join("stop.signal")
}

pub fn run(project_root: PathBuf, mode: RunMode) -> Result<(), String> {
    let project_root = project_root
        .canonicalize()
        .map_err(|e| format!("bad project path {}: {}", project_root.display(), e))?;
    if !project_root.join(".iter").is_dir() {
        return Err(format!("{} has no .iter/ directory (run `iter init`?)", project_root.display()));
    }
    let cfg = config::load(&project_root);

    // `iter run` clears any leftover stop signal.
    let _ = std::fs::remove_file(stop_signal_path(&project_root));

    let shared = Arc::new(Shared {
        project_root: project_root.clone(),
        cfg: cfg.clone(),
        queue_mutex: Mutex::new(()),
        stop_now: AtomicBool::new(false),
        deferred: Mutex::new(HashMap::new()),
    });

    // Startup crash recovery.
    {
        let _q = shared.queue_mutex.lock().unwrap();
        match workitems::recover_orphans(&shared.queue()) {
            Ok(0) => {}
            Ok(n) => logging::warn("engine", &format!("recovered {} orphaned in-progress item(s) back to queued", n)),
            Err(e) => logging::error("engine", &format!("orphan recovery failed: {}", e)),
        }
    }

    let mut running: Vec<(String, JoinHandle<()>)> = Vec::new();
    let mut worker_seq: u64 = 0;
    let mut stop_picking = false;
    let mut draining = false;
    let mut tick: u64 = 0;
    let mut last_summary = String::new();

    loop {
        tick += 1;

        // Stop signal: `drain` token = finish in-flight items; otherwise immediate.
        if !stop_picking {
            if let Ok(text) = std::fs::read_to_string(stop_signal_path(&project_root)) {
                stop_picking = true;
                draining = text.contains("drain");
                if draining {
                    logging::info("engine", "stop.signal (drain): finishing in-flight work, picking nothing new");
                } else {
                    logging::info("engine", "stop.signal: stopping; in-flight items will requeue after their current turn");
                    shared.stop_now.store(true, Ordering::SeqCst);
                }
            }
        }

        running.retain(|(_, h)| !h.is_finished());

        if !stop_picking {
            let agent_defs = agents::discover(&project_root);
            for agent in &agent_defs {
                loop {
                    let running_of_type = running.iter().filter(|(t, _)| t == &agent.type_name).count();
                    if running_of_type >= agent.max_agent_count || running.len() >= cfg.engine.max_total_agents {
                        break;
                    }
                    let Some(item) = pick_next(&shared, &agent.type_name) else { break };
                    worker_seq += 1;
                    let tag = format!("{}#{}", agent.type_name, worker_seq);
                    logging::info(
                        &tag,
                        &format!(
                            "picked {} \"{}\" (prio {}, source {}, attempt {})",
                            short(&item.workid),
                            item.title,
                            item.priority,
                            item.source,
                            item.attempts
                        ),
                    );
                    let shared2 = Arc::clone(&shared);
                    let agent2 = agent.clone();
                    let handle = std::thread::spawn(move || run_workitem(shared2, agent2, item, tag));
                    running.push((agent.type_name.clone(), handle));
                    std::thread::sleep(std::time::Duration::from_millis(cfg.engine.agent_stagger_ms));
                }
            }
        }

        // Tick summary (only when it changes, to keep the stream readable).
        let items = {
            let _q = shared.queue_mutex.lock().unwrap();
            shared.queue().load()
        };
        let summary = summarize(&items, running.len());
        if summary != last_summary {
            logging::info("engine", &format!("tick #{} — {}", tick, summary));
            last_summary = summary;
        }

        // Exit conditions.
        if stop_picking && running.is_empty() {
            logging::info("engine", if draining { "drained; engine stopped" } else { "engine stopped" });
            return Ok(());
        }
        if mode.once && tick >= 1 {
            while running.iter().any(|(_, h)| !h.is_finished()) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            logging::info("engine", "single tick complete (--once)");
            return Ok(());
        }
        if mode.until_idle && running.is_empty() {
            let now = chrono::Utc::now();
            let any_eligible = items.iter().any(|i| i.eligible(&cfg, now));
            if !any_eligible {
                logging::info("engine", "queue idle: no eligible work and nothing running (--until-idle)");
                return Ok(());
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(cfg.engine.tick_interval_sec.max(1)));
    }
}

fn summarize(items: &[WorkItem], running: usize) -> String {
    let count = |s: &str| items.iter().filter(|i| i.state == s).count();
    format!(
        "queue: {} open ({} queued, {} in-progress, {} paused, {} failed, {} todo); {} agent(s) running",
        items.len(),
        count(workitems::STATE_QUEUED),
        count(workitems::STATE_IN_PROGRESS),
        count(workitems::STATE_PAUSED),
        count(workitems::STATE_FAILED),
        count(workitems::STATE_TODO),
        running
    )
}

/// Select and claim the best eligible item of this type: mark it in-progress and
/// stamp times.start before releasing the queue mutex, so no other pick can race it.
fn pick_next(shared: &Shared, type_name: &str) -> Option<WorkItem> {
    let _q = shared.queue_mutex.lock().unwrap();
    let deferred = {
        let mut map = shared.deferred.lock().unwrap();
        let now = Instant::now();
        map.retain(|_, until| *until > now);
        map.keys().cloned().collect::<std::collections::HashSet<String>>()
    };
    let queue = shared.queue();
    let items = queue.load();
    let now = chrono::Utc::now();
    let mut best: Option<usize> = None;
    for (i, item) in items.iter().enumerate() {
        if item.item_type != type_name || !item.eligible(&shared.cfg, now) || deferred.contains(&item.workid) {
            continue;
        }
        best = match best {
            None => Some(i),
            Some(b) => {
                let (cur, cand) = (&items[b], item);
                let better = cand.effective_priority() > cur.effective_priority()
                    || (cand.effective_priority() == cur.effective_priority()
                        && cand.times.added < cur.times.added);
                if better { Some(i) } else { Some(b) }
            }
        };
    }
    let workid = items[best?].workid.clone();
    // Claim under the record lock so the API server / iter add can't race the pick.
    let claimed = queue.with_lock(|items| {
        let item = items.iter_mut().find(|i| i.workid == workid)?;
        if item.state != workitems::STATE_QUEUED && item.state != workitems::STATE_FAILED {
            return None; // someone changed it between our read and the lock
        }
        item.state = workitems::STATE_IN_PROGRESS.into();
        item.attempts += 1;
        item.times.start = workitems::now_iso();
        Some(item.clone())
    });
    match claimed {
        Ok(item) => item,
        Err(e) => {
            logging::error("engine", &format!("cannot claim workitem: {}", e));
            None
        }
    }
}

fn run_workitem(shared: Arc<Shared>, agent: AgentDef, item: WorkItem, tag: String) {
    let codepath = resolve_codepath(&shared.project_root, &item.codepath);
    let lock_timeout = shared.cfg.engine.codepath_lock_timeout_sec.max(agent.max_work_timeout_sec);

    // Codepath lock (see .iter/.engine/codepath_lock.md).
    let lock = match locks::acquire_codepath_lock(&codepath, &item.workid, &agent.type_name, lock_timeout) {
        Ok(lock) => {
            logging::info(&tag, &format!("codepath lock acquired: {}", codepath.display()));
            lock
        }
        Err(conflict) => {
            let backoff = shared.cfg.engine.codepath_conflict_backoff_sec;
            logging::info(
                &tag,
                &format!(
                    "codepath busy ({}); requeued {} (retry in {}s)",
                    conflict.display(),
                    short(&item.workid),
                    backoff
                ),
            );
            shared
                .deferred
                .lock()
                .unwrap()
                .insert(item.workid.clone(), Instant::now() + std::time::Duration::from_secs(backoff));
            requeue(&shared, &item.workid, "codepath lock conflict", true);
            return;
        }
    };

    let turns = build_turns(&shared, &agent, &item, &codepath, &tag);
    let mut session = Session::new(agent.clone(), codepath.clone(), shared.project_root.clone());
    let mut outputs: Vec<String> = Vec::new();

    for (i, step) in turns.iter().enumerate() {
        if shared.stop_now.load(Ordering::SeqCst) {
            logging::warn(&tag, "engine stopping: requeueing after current turn");
            requeue(&shared, &item.workid, "engine stopped mid-run; requeued", false);
            drop(lock);
            return;
        }
        match session.run(&step.turn) {
            Ok(result) => {
                logging::info(&tag, &format!("{} done", step.turn.label));
                outputs.push(format!("[{}] {}", step.turn.label, result));
                stamp_boundaries(&shared, &item.workid, &turns, i);
            }
            Err(e) => {
                let msg = format!("{} failed: {}", step.turn.label, e);
                logging::error(&tag, &msg);
                fail_item(&shared, &item, &msg, outputs.join("\n"), &tag);
                drop(lock);
                return;
            }
        }
    }

    // Close-out: complete → move to workitems_closed.jsonl.
    let output = outputs.join("\n");
    {
        let _q = shared.queue_mutex.lock().unwrap();
        let queue = shared.queue();
        let mut done = item.clone();
        let items = queue.load();
        if let Some(cur) = items.iter().find(|i| i.workid == item.workid) {
            done = cur.clone();
        }
        done.state = workitems::STATE_COMPLETE.into();
        done.output = output;
        done.times.closed = workitems::now_iso();
        fill_empty_stamps(&mut done.times);
        if let Err(e) = queue.close(&done) {
            logging::error(&tag, &format!("close-out failed: {}", e));
        }
    }
    logging::info(&tag, &format!("complete → workitems_closed.jsonl; lock released ({})", short(&item.workid)));
    drop(lock);
}

/// After turn `i` succeeds, stamp any phase boundary it completes.
fn stamp_boundaries(shared: &Shared, workid: &str, turns: &[StepTurn], i: usize) {
    let phase = turns[i].phase;
    let last_of_phase = turns.get(i + 1).map(|n| n.phase != phase).unwrap_or(true);
    if !last_of_phase || phase == Phase::SelfCheck {
        return;
    }
    let now = workitems::now_iso();
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| match phase {
        Phase::Prework => it.times.preworkdone = now.clone(),
        Phase::Mainwork => it.times.mainworkdone = now.clone(),
        Phase::Postwork => it.times.postworkdone = now.clone(),
        Phase::SelfCheck => {}
    });
}

fn fill_empty_stamps(times: &mut workitems::Times) {
    let closed = times.closed.clone();
    for stamp in [&mut times.preworkdone, &mut times.mainworkdone, &mut times.postworkdone] {
        if stamp.is_empty() {
            *stamp = closed.clone();
        }
    }
}

/// Put an item back in the queue. `refund_attempt` when the run never really started
/// (lock conflict) so contention doesn't burn attempts.
fn requeue(shared: &Shared, workid: &str, reason: &str, refund_attempt: bool) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| {
        it.state = workitems::STATE_QUEUED.into();
        it.lasterror = reason.into();
        if refund_attempt && it.attempts > 0 {
            it.attempts -= 1;
        }
        it.times.start = String::new();
    });
}

fn fail_item(shared: &Shared, item: &WorkItem, error: &str, partial_output: String, tag: &str) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let mut failed = item.clone();
    let items = queue.load();
    if let Some(cur) = items.iter().find(|i| i.workid == item.workid) {
        failed = cur.clone();
    }
    failed.state = workitems::STATE_FAILED.into();
    failed.lasterror = error.into();
    failed.output = partial_output;
    if failed.failed_terminally(&shared.cfg) {
        failed.times.closed = workitems::now_iso();
        fill_empty_stamps(&mut failed.times);
        if let Err(e) = queue.close(&failed) {
            logging::error(tag, &format!("terminal-failure close-out failed: {}", e));
        } else {
            logging::warn(tag, &format!("attempts exhausted → closed as failed ({})", short(&item.workid)));
        }
    } else {
        let attempts = failed.attempts;
        let _ = queue.mutate(&item.workid, |it| {
            it.state = workitems::STATE_FAILED.into();
            it.lasterror = error.into();
            it.output = failed.output.clone();
        });
        logging::warn(
            tag,
            &format!("failed (attempt {}/{}); retry after backoff", attempts, shared.cfg.engine.max_attempts),
        );
    }
}

fn resolve_codepath(project_root: &Path, codepath: &str) -> PathBuf {
    let mut p = codepath.to_string();
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            p = format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    let path = PathBuf::from(&p);
    let abs = if path.is_absolute() { path } else { project_root.join(path) };
    abs.canonicalize().unwrap_or(abs)
}

/// Compose the turn sequence: spin-up + first step, remaining prework, mainwork,
/// postwork, and the final self-check — per the engine-loop spec.
fn build_turns(shared: &Shared, agent: &AgentDef, item: &WorkItem, codepath: &Path, tag: &str) -> Vec<StepTurn> {
    let mut steps: Vec<StepTurn> = Vec::new();
    for entry in &item.prework {
        let (label, prompt) = resolve_prepost(&shared.project_root, entry, "prework");
        steps.push(StepTurn { phase: Phase::Prework, turn: Turn { label, prompt } });
    }
    steps.push(StepTurn {
        phase: Phase::Mainwork,
        turn: Turn { label: "mainwork".into(), prompt: item.mainwork.clone() },
    });
    for entry in &item.postwork {
        let (label, prompt) = resolve_prepost(&shared.project_root, entry, "postwork");
        steps.push(StepTurn { phase: Phase::Postwork, turn: Turn { label, prompt } });
    }
    steps.push(StepTurn {
        phase: Phase::SelfCheck,
        turn: Turn {
            label: "selfcheck".into(),
            prompt: format!(
                "Final check: re-read your agent definition below and confirm every \
                 instruction was completed for this work item. Report anything unfinished \
                 or skipped, or confirm all done.\n\n---\n{}",
                agent.body
            ),
        },
    });

    // Spin-up context is prepended to the FIRST turn so the whole run is one session.
    let (context_files, warnings) = context::resolve(&item.context, codepath, &shared.project_root);
    for w in &warnings {
        logging::warn(tag, w);
    }
    let mut spinup = String::new();
    spinup.push_str(&agent.body);
    if let Some(source_text) = source_instructions(&shared.project_root, &item.source) {
        spinup.push_str("\n\n# Source instructions\n");
        spinup.push_str(&source_text);
    }
    spinup.push_str(&format!(
        "\n\n# Work item\nTitle: {}\nWork item id: {}\nCodepath (your working directory and lock scope): {}\n",
        item.title,
        item.workid,
        codepath.display()
    ));
    if !context_files.is_empty() {
        spinup.push_str("\n# Context files\nRead each of these before starting:\n");
        for f in &context_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    if item.item_type.starts_with("test") && !item.testfiles.is_empty() {
        let (test_files, twarn) = context::resolve(&item.testfiles, codepath, &shared.project_root);
        for w in &twarn {
            logging::warn(tag, w);
        }
        spinup.push_str("\n# Test files\n");
        for f in &test_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    let first = &mut steps[0].turn;
    first.prompt = format!("{}\n\n# Step: {}\n{}", spinup, first.label, first.prompt);
    steps
}

/// Prepostwork resolution rule: the entry is a filename minus extension; if
/// `.iter/prepostwork/<entry>.md` exists its content is the prompt, otherwise the
/// entry itself is a literal inline prompt.
fn resolve_prepost(project_root: &Path, entry: &str, phase: &str) -> (String, String) {
    let file = project_root.join(".iter").join("prepostwork").join(format!("{}.md", entry));
    match std::fs::read_to_string(&file) {
        Ok(content) => (format!("{}:{}", phase, entry), content),
        Err(_) => {
            let short: String = entry.chars().take(30).collect();
            (format!("{}:inline({}…)", phase, short.trim_end()), entry.to_string())
        }
    }
}

/// Map workitem.source to its `.iter/source/*.md` instructions. For `agent: {type}`
/// sources, `{type}` inside the file body is replaced with the originating type.
fn source_instructions(project_root: &Path, source: &str) -> Option<String> {
    let dir = project_root.join(".iter").join("source");
    let (file, origin_type) = if source == "user" {
        ("user.md", None)
    } else if source == "error" {
        ("error.md", None)
    } else if let Some(rest) = source.strip_prefix("agent") {
        ("agent.md", Some(rest.trim_start_matches(':').trim().to_string()))
    } else {
        return None;
    };
    let text = std::fs::read_to_string(dir.join(file)).ok()?;
    Some(match origin_type {
        Some(t) if !t.is_empty() => text.replace("{type}", &t),
        _ => text,
    })
}

fn short(workid: &str) -> String {
    workid.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_backoff_defers_repick() {
        let root = std::env::temp_dir().join(format!("iterloop-defer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".iter/.engine")).unwrap();
        let shared = Shared {
            project_root: root.clone(),
            cfg: Config::default(),
            queue_mutex: Mutex::new(()),
            stop_now: AtomicBool::new(false),
            deferred: Mutex::new(HashMap::new()),
        };
        let item = WorkItem { workid: "w1".into(), item_type: "code".into(), ..Default::default() };
        shared.queue().append(&item).unwrap();

        // Deferred item is skipped...
        shared.deferred.lock().unwrap().insert("w1".into(), Instant::now() + std::time::Duration::from_secs(60));
        assert!(pick_next(&shared, "code").is_none(), "deferred item must not be re-picked");

        // ...and picked again once the backoff expires.
        shared.deferred.lock().unwrap().insert("w1".into(), Instant::now() - std::time::Duration::from_secs(1));
        let picked = pick_next(&shared, "code").expect("expired deferral must be pickable");
        assert_eq!(picked.workid, "w1");
        assert_eq!(picked.state, workitems::STATE_IN_PROGRESS);
        assert!(shared.deferred.lock().unwrap().is_empty(), "expired entries are pruned");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prepost_resolution_file_vs_inline() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let (label, prompt) = resolve_prepost(&root, "git-pull", "prework");
        assert_eq!(label, "prework:git-pull");
        assert!(prompt.contains("git pull --rebase"));

        let (label, prompt) = resolve_prepost(&root, "Just do this literal thing", "prework");
        assert!(label.starts_with("prework:inline("));
        assert_eq!(prompt, "Just do this literal thing");
    }

    #[test]
    fn source_instructions_substitution() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let user = source_instructions(&root, "user").unwrap();
        assert!(user.contains("human user"));
        let agent = source_instructions(&root, "agent: plan").unwrap();
        assert!(agent.contains("plan"), "{{type}} must be replaced");
        assert!(!agent.contains("{type}"));
        assert!(source_instructions(&root, "weird").is_none());
    }
}
