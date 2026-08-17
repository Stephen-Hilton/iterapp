use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::agents::{self, AgentDef};
use crate::config::{self, Config};
use crate::context;
use crate::limits;
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
    /// Re-loaded from config.json every tick, so Iterloop Settings changes
    /// (max_total_agents, backoffs, caps) apply live — no engine restart.
    cfg: Mutex<Config>,
    /// Serializes every load-modify-save sequence on the queue within this process.
    /// (The on-disk record lock protects against OTHER processes.)
    queue_mutex: Mutex<()>,
    /// Immediate-stop flag: workers requeue their item between turns when set.
    stop_now: AtomicBool,
    /// Codepath-conflict backoff: workid → don't re-pick before this instant. Purely
    /// in-memory noise suppression; a restart just retries sooner.
    deferred: Mutex<HashMap<String, Instant>>,
    /// Resolved lock scopes of items currently running in THIS engine
    /// (workid → (path, codepath_ignore)). pick_next skips candidates that overlap
    /// one, so an occupied lock scope never costs a pick; entries are removed when
    /// the worker thread finishes.
    running_paths: Mutex<HashMap<String, (PathBuf, Vec<String>)>>,
    /// Usage-limit hold: pick nothing until this instant (set by a worker that hit a
    /// window limit; lifted early when a fresh snapshot shows utilization back
    /// under 95%). Unlike the stop signal, the engine stays alive and auto-resumes.
    limit_hold: Mutex<Option<chrono::DateTime<chrono::Utc>>>,
}

/// RAII entry in Shared.running_paths: dropped (in the worker thread) when the run
/// ends by any path — completion, failure, requeue, or panic unwind.
struct PathClaim {
    shared: Arc<Shared>,
    workid: String,
}

impl Drop for PathClaim {
    fn drop(&mut self) {
        self.shared.running_paths.lock().unwrap().remove(&self.workid);
    }
}

/// True when one path contains the other (or they are equal) — the same overlap rule
/// the on-disk .iter.lock enforces between ancestors and descendants.
/// Do two lock scopes overlap? Same-or-nested paths overlap UNLESS the outer scope's
/// codepath_ignore patterns carve the inner path out (mirrors locks::ignored, which
/// governs the on-disk `.iter.lock` conflict rules).
fn scopes_overlap(a: &Path, a_ignore: &[String], b: &Path, b_ignore: &[String]) -> bool {
    if a == b {
        return true;
    }
    if let Ok(rel) = a.strip_prefix(b) {
        return !locks::ignored(rel, b_ignore); // a is inside b's scope unless carved out
    }
    if let Ok(rel) = b.strip_prefix(a) {
        return !locks::ignored(rel, a_ignore);
    }
    false
}

impl Shared {
    fn cfg(&self) -> Config {
        self.cfg.lock().unwrap().clone()
    }

    fn queue(&self) -> Queue {
        Queue::new(&self.project_root, &self.cfg())
    }
}

pub fn stop_signal_path(project_root: &Path) -> PathBuf {
    config::engine_dir(project_root).join("stop.signal")
}

/// Fail-flag written by `iter critreview` when a REQUESTED review could not be
/// delivered (critic crash or usage limit): the engine consumes it at the next
/// turn boundary and fails the work item deterministically, so a lost review is
/// a visible failure that retries — never a quiet "proceeded without review".
pub fn critfail_path(project_root: &Path, workid: &str) -> PathBuf {
    config::engine_dir(project_root).join(format!("critfail-{}.txt", workid))
}

fn take_critfail(project_root: &Path, workid: &str) -> Option<String> {
    let path = critfail_path(project_root, workid);
    let reason = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let reason = reason.trim();
    Some(if reason.is_empty() {
        "critical review failed (no reason recorded)".to_string()
    } else {
        reason.to_string()
    })
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
        cfg: Mutex::new(cfg),
        queue_mutex: Mutex::new(()),
        stop_now: AtomicBool::new(false),
        deferred: Mutex::new(HashMap::new()),
        running_paths: Mutex::new(HashMap::new()),
        limit_hold: Mutex::new(None),
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
    // Usage-tier throttle state (limits.rs).
    let mut last_tier_cap: Option<Option<usize>> = None;
    let mut stale_warned = false;
    let mut probe_last = Instant::now() - std::time::Duration::from_secs(24 * 3600);
    let mut probe_count: u64 = 0;
    let probe_in_flight = Arc::new(AtomicBool::new(false));
    // Deterministic test sweep (testsweep.rs): first pass ~90s after start (a
    // restart shouldn't immediately re-run every stale group), then on cadence.
    let sweep_interval = |cfg: &Config| {
        std::time::Duration::from_secs(cfg.testing.minutes_between_test_sweeps.max(1) * 60)
    };
    let mut sweep_last = Instant::now()
        .checked_sub(sweep_interval(&shared.cfg()).saturating_sub(std::time::Duration::from_secs(90)))
        .unwrap_or_else(Instant::now);
    let sweep_in_flight = Arc::new(AtomicBool::new(false));

    loop {
        tick += 1;

        // Live settings: re-read config.json each tick so webapp edits apply now.
        let cfg = config::load(&project_root);
        *shared.cfg.lock().unwrap() = cfg.clone();

        // Daily budget: at the cap, write a drain signal so in-flight work finishes
        // and nothing new starts. Raising the cap (or the next UTC day) lifts it.
        if !stop_picking && cfg.engine.max_cost_usd_per_day > 0.0 {
            let spent = crate::spend::today_usd(&project_root);
            if spent >= cfg.engine.max_cost_usd_per_day {
                logging::error(
                    "engine",
                    &format!(
                        "daily budget reached (${:.2} of ${:.2}) — draining; raise max_cost_usd_per_day in Settings to resume",
                        spent, cfg.engine.max_cost_usd_per_day
                    ),
                );
                let _ = std::fs::write(
                    stop_signal_path(&project_root),
                    format!("{} drain auto: daily budget reached (${:.2})\n", workitems::now_iso(), spent),
                );
            }
        }

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

        // Account-usage throttle: server-authoritative percentages from the
        // statusline snapshot cap concurrency in tiers (see limits.rs).
        let now_utc = chrono::Utc::now();
        let usage = limits::read_snapshot(&cfg);
        let pct = usage.as_ref().map(|u| u.effective_pct(now_utc));
        let tier = pct.and_then(|p| limits::tier_cap(&cfg.limits, p));
        if last_tier_cap != Some(tier) {
            match (tier, pct) {
                (Some(cap), Some(p)) => logging::warn(
                    "engine",
                    &format!("account usage {:.0}% → max agents capped at {}", p, cap),
                ),
                (None, Some(p)) => logging::info("engine", &format!("account usage {:.0}% — tier throttle off", p)),
                _ => {}
            }
            last_tier_cap = Some(tier);
        }
        let effective_max = tier.map_or(cfg.engine.max_total_agents, |c| c.min(cfg.engine.max_total_agents));

        // Usage-limit hold: set by a worker that hit a window limit. Lifted at the
        // recorded reset time, or early when fresh data shows usage back under 95%.
        let mut holding = false;
        {
            let mut hold = shared.limit_hold.lock().unwrap();
            if let Some(until) = *hold {
                let fresh_ok = usage.as_ref().is_some_and(|u| {
                    u.age_sec(now_utc) < (cfg.limits.probe_interval_sec as i64 * 2)
                        && u.effective_pct(now_utc) < 95.0
                });
                if now_utc >= until || fresh_ok {
                    logging::info("engine", "usage-limit hold lifted; resuming picking");
                    *hold = None;
                } else {
                    holding = true;
                }
            }
        }

        if !stop_picking && !holding {
            let agent_defs = agents::discover(&project_root);
            // Fill slots in global priority order: every pass offers the types that
            // still have a free per-type slot, and pick_next claims the single best
            // eligible item across all of them. The highest-priority item therefore
            // always starts first, regardless of its type, unless that type's
            // max_agent_count is already saturated (or zero).
            while running.len() < effective_max {
                let open_types: Vec<&str> = agent_defs
                    .iter()
                    .filter(|a| {
                        running.iter().filter(|(t, _)| t == &a.type_name).count() < a.max_agent_count
                    })
                    .map(|a| a.type_name.as_str())
                    .collect();
                if open_types.is_empty() {
                    break;
                }
                let Some(item) = pick_next(&shared, &open_types) else { break };
                let agent = agent_defs
                    .iter()
                    .find(|a| a.type_name == item.item_type)
                    .expect("picked item's type comes from agent_defs")
                    .clone();
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
                // Claim the resolved codepath before spawning, so a second pick in
                // this same tick already sees it as occupied.
                let resolved = resolve_codepath(&config::code_root(&shared.project_root, &cfg), &item.codepath);
                shared
                    .running_paths
                    .lock()
                    .unwrap()
                    .insert(item.workid.clone(), (resolved, item.codepath_ignore.clone()));
                let claim = PathClaim { shared: Arc::clone(&shared), workid: item.workid.clone() };
                let type_name = agent.type_name.clone();
                let handle = std::thread::spawn(move || {
                    let _claim = claim;
                    run_workitem(shared2, agent, item, tag);
                });
                running.push((type_name, handle));
                std::thread::sleep(std::time::Duration::from_millis(cfg.engine.agent_stagger_ms));
            }
        }

        // Deterministic test sweep, on cadence, in its own thread (a group's whole
        // run budget can be minutes — the tick loop must not stall). Draining or
        // holding engines don't sweep: results would only mint work nobody picks.
        if cfg.testing.test_sweep_active
            && !stop_picking
            && !holding
            && !sweep_in_flight.load(Ordering::SeqCst)
            && sweep_last.elapsed() >= sweep_interval(&cfg)
        {
            sweep_last = Instant::now();
            sweep_in_flight.store(true, Ordering::SeqCst);
            let sweep_root = project_root.clone();
            let sweep_cfg = cfg.clone();
            let flag = Arc::clone(&sweep_in_flight);
            std::thread::spawn(move || {
                logging::info("sweep", "test sweep starting");
                let report = crate::testsweep::sweep(&sweep_root, &sweep_cfg);
                logging::info("sweep", &report.summary());
                for note in &report.notes {
                    logging::info("sweep", note);
                }
                flag.store(false, Ordering::SeqCst);
            });
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

        // Usage probe + staleness: only relevant while there is work to run (or a
        // hold to lift). The probe pokes a background interactive claude session so
        // its statusline refreshes the snapshot; idle engines never poke.
        let work_present =
            !running.is_empty() || holding || items.iter().any(|i| i.eligible(&cfg, now_utc));
        if work_present && !stop_picking {
            let stale_sec = usage.as_ref().map(|u| u.age_sec(now_utc)).unwrap_or(i64::MAX);
            if stale_sec > cfg.limits.snapshot_stale_warn_sec as i64 {
                if !stale_warned {
                    let what = if usage.is_none() { "missing".to_string() } else { format!("{}s old", stale_sec) };
                    logging::warn(
                        "engine",
                        &format!("usage snapshot is {} — tier throttle runs on last known data{}", what,
                            if cfg.limits.probe_enabled { "" } else { " (limits.probe_enabled is off)" }),
                    );
                    stale_warned = true;
                }
            } else {
                stale_warned = false;
            }
            if cfg.limits.probe_enabled
                && stale_sec > cfg.limits.probe_interval_sec as i64
                && probe_last.elapsed().as_secs() >= cfg.limits.probe_interval_sec
                && !probe_in_flight.load(Ordering::SeqCst)
            {
                probe_last = Instant::now();
                probe_count += 1;
                probe_in_flight.store(true, Ordering::SeqCst);
                let probe_root = project_root.clone();
                let probe_cfg = cfg.clone();
                let flag = Arc::clone(&probe_in_flight);
                let n = probe_count;
                std::thread::spawn(move || {
                    limits::probe_poke(&probe_root, &probe_cfg, n);
                    flag.store(false, Ordering::SeqCst);
                });
            }
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

/// Select and claim the best eligible item whose type is in `allowed_types`:
/// mark it in-progress and stamp times.start before releasing the queue mutex,
/// so no other pick can race it. Candidates compete on effective_priority across
/// all allowed types, so the caller gets the globally best item, not the best of
/// one type.
fn pick_next(shared: &Shared, allowed_types: &[&str]) -> Option<WorkItem> {
    let cfg = shared.cfg();
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
    let code_root = config::code_root(&shared.project_root, &cfg);
    let occupied: Vec<(PathBuf, Vec<String>)> = shared.running_paths.lock().unwrap().values().cloned().collect();
    let mut best: Option<usize> = None;
    let mut best_repairs: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if !allowed_types.contains(&item.item_type.as_str())
            || !item.eligible(&cfg, now)
            || deferred.contains(&item.workid)
        {
            continue;
        }
        // Keep moving down the queue: a candidate that cannot run right now — its
        // codepath overlaps a running item's scope, or an on-disk lock (another
        // engine, or a leftover) covers it — is skipped, not picked, so it never
        // wastes the free agent slot. It's reconsidered fresh every tick.
        let cand_path = resolve_codepath(&code_root, &item.codepath);
        // Disjointness guard (features/TDD.md step 4): a code item whose scope
        // swallows an OPEN testwriter item's scope is misconfigured — the plan
        // agent forgot codepath_ignore. Repair deterministically (carve the test
        // subtree out) instead of trusting prompts; the repair also applies to the
        // overlap check below so the pair can actually run in parallel.
        let repairs = disjointness_repairs(item, &cand_path, &items, &code_root, &cfg.globalsettings.test_dir);
        let effective_ignore: Vec<String> =
            item.codepath_ignore.iter().cloned().chain(repairs.iter().cloned()).collect();
        if occupied.iter().any(|(r, r_ign)| scopes_overlap(r, r_ign, &cand_path, &effective_ignore)) {
            continue;
        }
        if locks::find_ancestor_lock(&cand_path, now).is_some() {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let cur = &items[b];
                item.effective_priority() > cur.effective_priority()
                    || (item.effective_priority() == cur.effective_priority()
                        && item.times.added < cur.times.added)
            }
        };
        if better {
            best = Some(i);
            best_repairs = repairs;
        }
    }
    let workid = items[best?].workid.clone();
    if !best_repairs.is_empty() {
        logging::warn(
            "engine",
            &format!(
                "disjointness guard: code item {} scope covered an open testwriter scope; auto-added codepath_ignore {:?}",
                short(&workid),
                best_repairs
            ),
        );
    }
    // Claim under the record lock so the API server / iter add can't race the pick.
    let claimed = queue.with_lock(|items| {
        let item = items.iter_mut().find(|i| i.workid == workid)?;
        if item.state != workitems::STATE_QUEUED && item.state != workitems::STATE_FAILED {
            return None; // someone changed it between our read and the lock
        }
        item.state = workitems::STATE_IN_PROGRESS.into();
        item.attempts += 1;
        item.times.start = workitems::now_iso();
        item.codepath_ignore.extend(best_repairs.iter().cloned());
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

/// The ignore patterns a `code` candidate is missing: one per open testwriter item
/// whose TEST-DIR scope (`…/<test_dir>`, per globalsettings.test_dir) sits strictly
/// inside the candidate's scope without being carved out. Tests belong to the
/// testwriter — code items must never lock (or edit) them. Deliberately narrow:
/// only test-dir-named scopes are carved, so a testwriter item with an odd codepath
/// can never strip a code item of its own sources.
fn disjointness_repairs(
    item: &WorkItem,
    cand_path: &Path,
    items: &[WorkItem],
    code_root: &Path,
    test_dir: &str,
) -> Vec<String> {
    if item.item_type != "code" {
        return Vec::new();
    }
    let mut repairs = Vec::new();
    for other in items {
        if other.item_type != "testwriter" || other.workid == item.workid {
            continue;
        }
        let tw_path = resolve_codepath(code_root, &other.codepath);
        if tw_path.file_name().map(|f| f != test_dir).unwrap_or(true) {
            continue;
        }
        if let Ok(rel) = tw_path.strip_prefix(cand_path) {
            if !rel.as_os_str().is_empty() && !locks::ignored(rel, &item.codepath_ignore) {
                let pat = format!("{}/", rel.to_string_lossy());
                if !repairs.contains(&pat) {
                    repairs.push(pat);
                }
            }
        }
    }
    repairs
}

fn run_workitem(shared: Arc<Shared>, agent: AgentDef, item: WorkItem, tag: String) {
    let cfg = shared.cfg();
    let code_root = config::code_root(&shared.project_root, &cfg);
    let codepath = resolve_codepath(&code_root, &item.codepath);
    let lock_timeout = cfg.engine.codepath_lock_timeout_sec.max(agent.max_work_timeout_sec);

    // A codepath that doesn't exist is a broken work item, not a busy one: fail it
    // (normal attempt/backoff rules apply) instead of requeueing forever.
    if !codepath.is_dir() {
        let msg = format!(
            "codepath does not exist: {} (stored \"{}\", resolved against code_root {})",
            codepath.display(),
            item.codepath,
            code_root.display()
        );
        logging::error(&tag, &msg);
        fail_item(&shared, &item, &msg, String::new(), &tag);
        return;
    }

    // Codepath lock (see .iter/.engine/codepath_lock.md).
    let lock = match locks::acquire_codepath_lock(&codepath, &item.workid, &agent.type_name, lock_timeout, &item.codepath_ignore) {
        Ok(lock) => {
            logging::info(&tag, &format!("codepath lock acquired: {}", codepath.display()));
            lock
        }
        Err(conflict) => {
            let backoff = cfg.engine.codepath_conflict_backoff_sec;
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
    // The workid lets an `iter critreview` subprocess flag THIS item as failed
    // deterministically (fail-flag file) instead of trusting the agent to stop.
    session.envs.push(("ITER_WORKID".to_string(), item.workid.clone()));
    let _ = std::fs::remove_file(critfail_path(&shared.project_root, &item.workid)); // stale flag from a killed prior attempt
    let mut outputs: Vec<String> = Vec::new();

    for (i, step) in turns.iter().enumerate() {
        if shared.stop_now.load(Ordering::SeqCst) {
            logging::warn(&tag, "engine stopping: requeueing after current turn");
            requeue_with_output(&shared, &item.workid, "engine stopped mid-run; requeued", false, Some(outputs.join("\n")));
            drop(lock);
            return;
        }
        let mut turn_result = session.run(&step.turn);
        if let Ok(outcome) = &turn_result {
            crate::spend::record(&shared.project_root, &crate::spend::SpendEntry {
                ts: workitems::now_iso(),
                workid: item.workid.clone(),
                agent: agent.type_name.clone(),
                turn: step.turn.label.clone(),
                usd: outcome.cost_usd,
                input_tokens: outcome.input_tokens,
                output_tokens: outcome.output_tokens,
            });
            logging::info(&tag, &format!("{} done (${:.2})", step.turn.label, outcome.cost_usd));
            outputs.push(format!("[{}] {}", step.turn.label, outcome.text));
            stamp_boundaries(&shared, &item.workid, &turns, i);
            // A critreview subprocess may have flagged this item as failed (critic
            // crash or usage limit). Consume the flag and fail the turn regardless
            // of what the agent's own output claims — a requested review that never
            // happened must surface as a failed item, not a quiet success.
            if let Some(reason) = take_critfail(&shared.project_root, &item.workid) {
                logging::warn(&tag, &format!("critreview flagged failure: {}", reason));
                turn_result = Err(reason);
            }
        }
        match turn_result {
            Ok(_) => {}
            Err(e) => {
                // Account limits fail every turn that would follow. Billing/account
                // states drain the engine (nothing resets on its own); time-window
                // limits enter a hold that auto-resumes at the reset.
                if crate::spend::is_usage_limit_error(&e) {
                    if crate::spend::is_account_terminal_error(&e) {
                        logging::error(&tag, &format!("credit/account limit hit ({}); auto-draining engine and requeueing {}", e, short(&item.workid)));
                        let _ = std::fs::write(
                            stop_signal_path(&shared.project_root),
                            format!("{} drain auto: credit/account limit reached\n", workitems::now_iso()),
                        );
                        requeue_with_output(&shared, &item.workid, "credit/account limit reached; engine auto-drained", true, Some(outputs.join("\n")));
                    } else {
                        let now = chrono::Utc::now();
                        let retry = cfg.limits.probe_interval_sec.max(60) as i64;
                        let until = crate::limits::parse_reset_epoch(&e, now)
                            .unwrap_or_else(|| now + chrono::Duration::seconds(retry));
                        logging::error(&tag, &format!(
                            "usage window limit hit ({}); holding new picks until {} and requeueing {}",
                            e,
                            until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            short(&item.workid)
                        ));
                        *shared.limit_hold.lock().unwrap() = Some(until);
                        requeue_with_output(&shared, &item.workid, "usage limit reached; engine holding until reset", true, Some(outputs.join("\n")));
                    }
                    drop(lock);
                    return;
                }
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
    requeue_with_output(shared, workid, reason, refund_attempt, None);
}

/// Mid-run requeues (engine stop, usage-limit hold/drain) pass the turns completed
/// so far, so the next attempt starts with the previous attempt's context instead
/// of rediscovering everything from zero.
fn requeue_with_output(shared: &Shared, workid: &str, reason: &str, refund_attempt: bool, partial_output: Option<String>) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| {
        it.state = workitems::STATE_QUEUED.into();
        it.lasterror = reason.into();
        if let Some(out) = &partial_output {
            if !out.is_empty() {
                it.output = out.clone();
            }
        }
        if refund_attempt && it.attempts > 0 {
            it.attempts -= 1;
        }
        it.times.start = String::new();
    });
}

fn fail_item(shared: &Shared, item: &WorkItem, error: &str, partial_output: String, tag: &str) {
    let cfg = shared.cfg();
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
    if failed.failed_terminally(&cfg) {
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
            &format!("failed (attempt {}/{}); retry after backoff", attempts, cfg.engine.max_attempts),
        );
    }
}

/// Longest partial-output tail shown to a retry; older turns matter less than the
/// end of the transcript, and unbounded output would crowd out the actual work.
const PREV_OUTPUT_TAIL_CHARS: usize = 4000;

/// Retry context prepended to a re-picked item's spin-up: what the last attempt
/// died of, plus the tail of what it produced. Without this a retry starts blind
/// and tends to repeat the same path into the same wall.
fn previous_attempt_section(item: &WorkItem) -> String {
    if item.lasterror.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "\n# Previous attempt\nThis work item ran before and did not complete. Last error: {}\n",
        item.lasterror
    );
    let out = item.output.trim();
    if !out.is_empty() {
        let start = out
            .char_indices()
            .rev()
            .nth(PREV_OUTPUT_TAIL_CHARS.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(0);
        s.push_str(&format!(
            "Partial output of the previous attempt{}:\n{}\n",
            if start > 0 { " (tail)" } else { "" },
            &out[start..]
        ));
    }
    s
}

/// Resolve a stored codepath to an absolute directory. `base` is the configured
/// code_root (which defaults to the engine home).
fn resolve_codepath(base: &Path, codepath: &str) -> PathBuf {
    let mut p = codepath.to_string();
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            p = format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    let path = PathBuf::from(&p);
    let abs = if path.is_absolute() { path } else { base.join(path) };
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
    let shared_text = agents::shared_instructions(&shared.project_root);
    let shared_section = shared_text
        .as_deref()
        .map(|t| format!("\n\n# Shared instructions (all agents — .iter/agents/_shared.md)\n{}", t))
        .unwrap_or_default();
    steps.push(StepTurn {
        phase: Phase::SelfCheck,
        turn: Turn {
            label: "selfcheck".into(),
            prompt: format!(
                "Final check: re-read your agent definition below and confirm every \
                 instruction was completed for this work item. Report anything unfinished \
                 or skipped, or confirm all done.\n\n---\n{}{}",
                agent.body, shared_section
            ),
        },
    });

    // Spin-up context is prepended to the FIRST turn so the whole run is one session.
    // Relative context/testfile patterns resolve against code_root, like codepaths;
    // agent/source/prepostwork definitions stay with the engine home (.iter/).
    let code_root = config::code_root(&shared.project_root, &shared.cfg());
    let reqs_dir = context::reqs_dir(&shared.project_root, &code_root);
    let (context_files, warnings) = context::resolve(&item.context, codepath, &code_root, &reqs_dir);
    for w in &warnings {
        logging::warn(tag, w);
    }
    let mut spinup = String::new();
    spinup.push_str(&agent.body);
    spinup.push_str(&shared_section);
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
    spinup.push_str(&previous_attempt_section(item));
    // Project-wide requirements are surfaced to EVERY work item — that is what makes
    // the reqs dir first-class. Files already attached as context aren't repeated.
    let reqs_files: Vec<_> =
        context::reqs_files(&reqs_dir).into_iter().filter(|f| !context_files.contains(f)).collect();
    if !reqs_files.is_empty() {
        spinup.push_str(&format!(
            "\n# Project-wide requirements ($ITER_REQS = {})\nGlobal requirements for the whole project — read what applies before starting:\n",
            reqs_dir.display()
        ));
        for f in &reqs_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    if !context_files.is_empty() {
        spinup.push_str("\n# Context files\nRead each of these before starting:\n");
        for f in &context_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    if item.item_type.starts_with("test") && !item.testfiles.is_empty() {
        let (test_files, twarn) = context::resolve(&item.testfiles, codepath, &code_root, &reqs_dir);
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
    fn previous_attempt_section_gives_retries_context() {
        // Fresh item: no section.
        let fresh = WorkItem::default();
        assert!(previous_attempt_section(&fresh).is_empty());

        // Failed item: error and full output included.
        let failed = WorkItem {
            lasterror: "mainwork failed: critical review failed after 2 attempt(s): boom".into(),
            output: "[mainwork] built half the thing".into(),
            ..Default::default()
        };
        let s = previous_attempt_section(&failed);
        assert!(s.contains("# Previous attempt"));
        assert!(s.contains("critical review failed"));
        assert!(s.contains("built half the thing"));
        assert!(!s.contains("(tail)"), "short output is not marked truncated");

        // Long output: only the tail survives, marked as such.
        let long = WorkItem {
            lasterror: "err".into(),
            output: format!("{}END-MARKER", "x".repeat(10_000)),
            ..Default::default()
        };
        let s = previous_attempt_section(&long);
        assert!(s.contains("(tail)") && s.contains("END-MARKER"));
        assert!(s.len() < 5000, "tail is bounded, got {}", s.len());
    }

    #[test]
    fn conflict_backoff_defers_repick() {
        let root = std::env::temp_dir().join(format!("iterloop-defer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".iter/.engine")).unwrap();
        let shared = test_shared(&root);
        let item = WorkItem { workid: "w1".into(), item_type: "code".into(), ..Default::default() };
        shared.queue().append(&item).unwrap();

        // Deferred item is skipped...
        shared.deferred.lock().unwrap().insert("w1".into(), Instant::now() + std::time::Duration::from_secs(60));
        assert!(pick_next(&shared, &["code"]).is_none(), "deferred item must not be re-picked");

        // ...and picked again once the backoff expires.
        shared.deferred.lock().unwrap().insert("w1".into(), Instant::now() - std::time::Duration::from_secs(1));
        let picked = pick_next(&shared, &["code"]).expect("expired deferral must be pickable");
        assert_eq!(picked.workid, "w1");
        assert_eq!(picked.state, workitems::STATE_IN_PROGRESS);
        assert!(shared.deferred.lock().unwrap().is_empty(), "expired entries are pruned");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn test_shared(root: &Path) -> Shared {
        std::fs::create_dir_all(root.join(".iter/.engine")).unwrap();
        Shared {
            project_root: root.to_path_buf(),
            cfg: Mutex::new(Config::default()),
            queue_mutex: Mutex::new(()),
            stop_now: AtomicBool::new(false),
            deferred: Mutex::new(HashMap::new()),
            running_paths: Mutex::new(HashMap::new()),
            limit_hold: Mutex::new(None),
        }
    }

    #[test]
    fn pick_skips_occupied_and_locked_codepaths() {
        let root = std::env::temp_dir().join(format!("iterloop-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["a", "a/deep", "b", "c"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let shared = test_shared(&root);
        let mk = |id: &str, prio: i64, cp: &str| WorkItem {
            workid: id.into(),
            item_type: "code".into(),
            priority: prio,
            codepath: cp.into(),
            ..Default::default()
        };
        // Highest prio overlaps a running item's scope; next is under an on-disk
        // lock; the third must be picked even though it sorts last.
        shared.queue().append(&mk("w-occupied", 9, "a/deep")).unwrap();
        shared.queue().append(&mk("w-locked", 8, "b")).unwrap();
        shared.queue().append(&mk("w-free", 1, "c")).unwrap();
        shared
            .running_paths
            .lock()
            .unwrap()
            .insert("running".into(), (root.join("a").canonicalize().unwrap(), Vec::new()));
        let _lock = locks::acquire_codepath_lock(&root.join("b"), "other-engine", "code", 600, &[]).unwrap();
        let picked = pick_next(&shared, &["code"]).expect("must keep moving down the queue");
        assert_eq!(picked.workid, "w-free");
        // Nothing else runnable now.
        assert!(pick_next(&shared, &["code"]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_allows_item_inside_running_scopes_ignored_subtree() {
        let root = std::env::temp_dir().join(format!("iterloop-cpign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("obj/test")).unwrap();
        std::fs::create_dir_all(root.join("obj/src")).unwrap();
        let shared = test_shared(&root);
        // A code item is running on obj/ but its lock scope carves out test/.
        shared
            .running_paths
            .lock()
            .unwrap()
            .insert("running-code".into(), (root.join("obj").canonicalize().unwrap(), vec!["test/".into()]));
        // A testwriter item scoped to the carved-out subtree is runnable NOW…
        let tw = WorkItem {
            workid: "w-tw".into(),
            item_type: "testwriter".into(),
            priority: 5,
            codepath: "obj/test".into(),
            ..Default::default()
        };
        // …while a sibling touching non-ignored code is not.
        let clash = WorkItem {
            workid: "w-clash".into(),
            item_type: "testwriter".into(),
            priority: 9,
            codepath: "obj/src".into(),
            ..Default::default()
        };
        shared.queue().append(&clash).unwrap();
        shared.queue().append(&tw).unwrap();
        let picked = pick_next(&shared, &["testwriter"]).expect("ignored subtree must be pickable");
        assert_eq!(picked.workid, "w-tw");
        assert!(pick_next(&shared, &["testwriter"]).is_none(), "non-ignored sibling stays blocked");

        // Mirror image: testwriter running on obj/test; a code item on obj/ that
        // ignores test/ is pickable. One that doesn't gets REPAIRED by the
        // disjointness guard (test/ carved out deterministically) and then runs
        // in parallel too — misconfigured pairs are fixed, not trusted.
        shared.running_paths.lock().unwrap().clear();
        shared
            .running_paths
            .lock()
            .unwrap()
            .insert("running-tw".into(), (root.join("obj/test").canonicalize().unwrap(), Vec::new()));
        let code_ign = WorkItem {
            workid: "w-code-ign".into(),
            item_type: "code".into(),
            priority: 5,
            codepath: "obj".into(),
            codepath_ignore: vec!["test/".into()],
            ..Default::default()
        };
        let code_full = WorkItem {
            workid: "w-code-full".into(),
            item_type: "code".into(),
            priority: 9,
            codepath: "obj".into(),
            ..Default::default()
        };
        shared.queue().append(&code_full).unwrap();
        shared.queue().append(&code_ign).unwrap();
        let picked = pick_next(&shared, &["code"]).expect("guard-repaired item must be pickable");
        assert_eq!(picked.workid, "w-code-full", "higher priority wins once the guard repairs its scope");
        assert!(
            picked.codepath_ignore.contains(&"test/".to_string()),
            "the repair is persisted on the claimed item: {:?}",
            picked.codepath_ignore
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn disjointness_guard_carves_only_test_dirs() {
        let root = std::env::temp_dir().join(format!("iterloop-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("obj/test")).unwrap();
        std::fs::create_dir_all(root.join("obj/src")).unwrap();
        let code_root = root.canonicalize().unwrap();
        let code = WorkItem { workid: "w-code".into(), item_type: "code".into(), codepath: "obj".into(), ..Default::default() };
        let tw_test = WorkItem { workid: "w-tw".into(), item_type: "testwriter".into(), codepath: "obj/test".into(), ..Default::default() };
        let tw_weird = WorkItem { workid: "w-weird".into(), item_type: "testwriter".into(), codepath: "obj/src".into(), ..Default::default() };
        let items = vec![code.clone(), tw_test, tw_weird];
        let cand_path = resolve_codepath(&code_root, "obj");
        let repairs = disjointness_repairs(&code, &cand_path, &items, &code_root, "test");
        assert_eq!(repairs, vec!["test/".to_string()], "test dir carved, src/ never");
        // Already-ignored scope needs no repair; non-code items are never repaired.
        let mut ignoring = code.clone();
        ignoring.codepath_ignore = vec!["test/".into()];
        assert!(disjointness_repairs(&ignoring, &cand_path, &items, &code_root, "test").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pick_is_global_priority_order_across_types() {
        let root = std::env::temp_dir().join(format!("iterloop-global-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["a", "b", "c"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let shared = test_shared(&root);
        let mk = |id: &str, ty: &str, prio: i64, cp: &str| WorkItem {
            workid: id.into(),
            item_type: ty.into(),
            priority: prio,
            codepath: cp.into(),
            ..Default::default()
        };
        shared.queue().append(&mk("w-code", "code", 5, "a")).unwrap();
        shared.queue().append(&mk("w-refactor", "refactor", 60, "b")).unwrap();
        shared.queue().append(&mk("w-ingest", "ingest", 6, "c")).unwrap();
        // Priority 60 wins even though "code" sorts first alphabetically.
        let types = ["code", "ingest", "refactor"];
        assert_eq!(pick_next(&shared, &types).unwrap().workid, "w-refactor");
        assert_eq!(pick_next(&shared, &types).unwrap().workid, "w-ingest");
        // With refactor's slot no longer offered, only code remains.
        assert_eq!(pick_next(&shared, &["code", "ingest"]).unwrap().workid, "w-code");
        assert!(pick_next(&shared, &types).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_codepath_fails_item_instead_of_requeueing() {
        let root = std::env::temp_dir().join(format!("iterloop-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shared = Arc::new(test_shared(&root));
        let item = WorkItem {
            workid: "w-ghostpath".into(),
            item_type: "code".into(),
            codepath: "does/not/exist".into(),
            attempts: 1,
            ..Default::default()
        };
        shared.queue().append(&item).unwrap();
        run_workitem(Arc::clone(&shared), AgentDef::default(), item, "test#1".into());
        let items = shared.queue().load();
        assert_eq!(items[0].state, workitems::STATE_FAILED, "must fail, not requeue");
        assert!(items[0].lasterror.contains("codepath does not exist"));
        assert!(items[0].lasterror.contains("does/not/exist"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn code_root_setting_rebases_relative_codepaths() {
        let root = std::env::temp_dir().join(format!("iterloop-croot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let engine_home = root.join("devops");
        std::fs::create_dir_all(engine_home.join(".iter/.engine")).unwrap();
        std::fs::create_dir_all(root.join("core/repos/comp")).unwrap();

        // Default: relative codepaths resolve against the engine home.
        let cfg = Config::default();
        let base = config::code_root(&engine_home, &cfg);
        assert_eq!(base, engine_home.canonicalize().unwrap());

        // code_root ".." rebases them to the parent — iter-in-a-subdir layout.
        let mut cfg = Config::default();
        cfg.globalsettings.code_root = "..".into();
        let base = config::code_root(&engine_home, &cfg);
        assert_eq!(base, root.canonicalize().unwrap());
        let resolved = resolve_codepath(&base, "core/repos/comp");
        assert_eq!(resolved, root.join("core/repos/comp").canonicalize().unwrap());
        // Absolute codepaths ignore the base entirely.
        let abs = resolve_codepath(&base, &engine_home.to_string_lossy());
        assert_eq!(abs, engine_home.canonicalize().unwrap());
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
