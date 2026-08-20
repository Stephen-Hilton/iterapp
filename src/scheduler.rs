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
    /// Set when this step is an engine-run shell command (a `.sh` prepostwork
    /// entry inside an agent item): run deterministically, no LLM turn; its
    /// output feeds the next LLM turn's prompt.
    shell: Option<String>,
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

/// Reject-flag written by `iter reject`: the agent judged the WORK invalid (out
/// of scope, unclear, premise broken) — not that it failed at the work. The
/// engine consumes it at the turn boundary and moves the item to `todo` with
/// the reason recorded: the high-attention, human-review bucket, where the item
/// can be edited and requeued — never retried automatically, never buried in
/// the completed archive.
pub fn reject_path(project_root: &Path, workid: &str) -> PathBuf {
    config::engine_dir(project_root).join(format!("reject-{}.txt", workid))
}

fn take_reject(project_root: &Path, workid: &str) -> Option<String> {
    let path = reject_path(project_root, workid);
    let reason = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let reason = reason.trim();
    Some(if reason.is_empty() { "rejected (no reason recorded)".to_string() } else { reason.to_string() })
}

/// User-stop flag written by the webapp's Stop action on an IN-PROGRESS item
/// ("errantly started" work). The runner polls for it mid-turn and kills the
/// session; the worker consumes it and moves the item to `todo` — partially
/// completed work needs human review, never an automatic retry.
pub fn stopitem_path(project_root: &Path, workid: &str) -> PathBuf {
    config::engine_dir(project_root).join(format!("stopitem-{}.signal", workid))
}

fn take_stopitem(project_root: &Path, workid: &str) -> bool {
    let path = stopitem_path(project_root, workid);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        return true;
    }
    false
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
    // itersched (itersched.rs): fires due `scheduled` templates into the queue.
    // The deterministic test sweep runs THROUGH this path — a "Test Loop"
    // scheduled workitem whose mainwork invokes `iter testsweep` (created from
    // the webapp's Test settings section); the engine has no sweep loop of its own.
    // First check on the first tick — restart memory is sched.last_fired on the
    // templates themselves, and daily/weekly occurrences missed while down are
    // skipped by the occurrence window, so an early check can never backfill.
    let mut sched_last = Instant::now()
        .checked_sub(std::time::Duration::from_secs(crate::itersched::CHECK_INTERVAL_SEC))
        .unwrap_or_else(Instant::now);

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

        // itersched: fire due schedules BEFORE picking, so a clone born this
        // tick can start this tick. Minute granularity — 59s cadence.
        if !stop_picking && !holding && sched_last.elapsed().as_secs() >= crate::itersched::CHECK_INTERVAL_SEC {
            sched_last = Instant::now();
            let _q = shared.queue_mutex.lock().unwrap();
            let fired = crate::itersched::check(&project_root, &cfg);
            if !fired.is_empty() {
                logging::info("sched", &format!("{} schedule(s) fired this pass", fired.len()));
            }
        }

        // Dependency housekeeping: a queued item whose dependency closed
        // FAILED (or vanished) can never dispatch — flip it to `todo` with the
        // blocker named, so a human reviews it instead of a silent hang, and
        // it never runs on a broken foundation.
        if !stop_picking {
            let _q = shared.queue_mutex.lock().unwrap();
            for (workid, why) in flip_failed_dependents(&shared.queue()) {
                logging::warn("engine", &format!("{} → todo: {}", short(&workid), why));
            }
        }

        if !stop_picking && !holding {
            let agent_defs = agents::discover(&project_root);
            // Fill slots in global priority order: every pass offers the types that
            // still have a free per-type slot, and pick_next claims the single best
            // eligible item across all of them. The most urgent item (lowest P —
            // priorities are lower-is-sooner) therefore always starts first,
            // regardless of its type, unless that type's max_agent_count is
            // already saturated (or zero).
            while running.len() < effective_max {
                let open_types: Vec<&str> = agent_defs
                    .iter()
                    .filter(|a| {
                        running.iter().filter(|(t, _)| t == &a.type_name).count() < a.max_agent_count
                    })
                    .map(|a| a.type_name.as_str())
                    .collect();
                // exec:"shell" items run engine-side (no agent, no LLM) under
                // their own concurrency cap, independent of agent-type slots.
                let allow_shell = running.iter().filter(|(t, _)| t == "shell").count()
                    < cfg.engine.max_shell_workers;
                if open_types.is_empty() && !allow_shell {
                    break;
                }
                let Some(item) = pick_next(&shared, &open_types, allow_shell) else { break };
                worker_seq += 1;
                let is_shell = item.exec == workitems::EXEC_SHELL;
                let type_name = if is_shell { "shell".to_string() } else { item.item_type.clone() };
                let tag = format!("{}#{}", type_name, worker_seq);
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
                let handle = if is_shell {
                    std::thread::spawn(move || {
                        let _claim = claim;
                        run_shell_workitem(shared2, item, tag);
                    })
                } else {
                    let agent = agent_defs
                        .iter()
                        .find(|a| a.type_name == item.item_type)
                        .expect("picked agent item's type comes from agent_defs")
                        .clone();
                    std::thread::spawn(move || {
                        let _claim = claim;
                        run_workitem(shared2, agent, item, tag);
                    })
                };
                running.push((type_name, handle));
                std::thread::sleep(std::time::Duration::from_millis(cfg.engine.agent_stagger_ms));
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

/// A FAILED dependency never releases the dependent: queued (and retryable
/// failed) items whose dependency can never satisfy — closed failed, or
/// missing — flip to `todo` with a note naming the failed dependency, the same
/// human-review bucket `iter reject` uses (failure births reviewable work).
/// `todo` items with dependencies are untouched: the gate applies to dispatch
/// of QUEUED items only. Returns the (workid, reason) pairs flipped.
fn flip_failed_dependents(queue: &Queue) -> Vec<(String, String)> {
    let gated = |i: &WorkItem| {
        !i.depends_on.is_empty()
            && (i.state == workitems::STATE_QUEUED || i.state == workitems::STATE_FAILED)
    };
    let items = queue.load();
    if !items.iter().any(gated) {
        return Vec::new();
    }
    let closed = queue.load_closed();
    let flips: Vec<(String, String)> = items
        .iter()
        .filter(|i| gated(i))
        .filter_map(|i| match workitems::dep_status(i, &items, &closed) {
            workitems::DepStatus::Failed(why) => Some((i.workid.clone(), why)),
            _ => None,
        })
        .collect();
    if flips.is_empty() {
        return flips;
    }
    let apply = flips.clone();
    let _ = queue.with_lock(move |items| {
        for (workid, why) in &apply {
            if let Some(it) = items.iter_mut().find(|i| i.workid == *workid) {
                if it.state == workitems::STATE_QUEUED || it.state == workitems::STATE_FAILED {
                    it.state = workitems::STATE_TODO.into();
                    it.lasterror = format!("DEPENDENCY FAILED: {}", why);
                    it.times.start = String::new();
                }
            }
        }
    });
    flips
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
fn pick_next(shared: &Shared, allowed_types: &[&str], allow_shell: bool) -> Option<WorkItem> {
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
    // Closed archive, loaded once per pick and only if a candidate has deps.
    let mut closed_cache: Option<Vec<WorkItem>> = None;
    let mut best: Option<usize> = None;
    let mut best_repairs: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        // shell items need no agent slot — their own cap gates them instead.
        let slot_ok = if item.exec == workitems::EXEC_SHELL {
            allow_shell
        } else {
            allowed_types.contains(&item.item_type.as_str())
        };
        if !slot_ok || !item.eligible(&cfg, now) || deferred.contains(&item.workid) {
            continue;
        }
        // Dependency gate (features/workitem_dependency.md): evaluated BEFORE
        // the lock checks. An unsatisfied dependency keeps the item visibly
        // queued; a FAILED dependency is handled by flip_failed_dependents at
        // the tick boundary, so both just skip here.
        if !item.depends_on.is_empty() {
            let closed = closed_cache.get_or_insert_with(|| queue.load_closed());
            if workitems::dep_status(item, &items, closed) != workitems::DepStatus::Satisfied {
                continue;
            }
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
                item.effective_priority() < cur.effective_priority() // lower = sooner (P0 most urgent)
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
        // Stale user-stop flags (crashed/failed prior attempt) die at claim,
        // INSIDE the record lock: a legit stop can only be written after the
        // in-progress state is visible on disk, which is strictly after this
        // removal — so no fresh stop can ever be lost here.
        let _ = std::fs::remove_file(stopitem_path(&shared.project_root, &item.workid));
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

    // Undo point for a mid-stream user stop.
    record_git_baseline(&shared, &item.workid, &codepath);

    let turns = build_turns(&shared, &agent, &item, &codepath, &tag);
    let mut session = Session::new(agent.clone(), codepath.clone(), shared.project_root.clone());
    // The workid lets an `iter critreview` subprocess flag THIS item as failed
    // deterministically (fail-flag file) instead of trusting the agent to stop.
    session.envs.push(("ITER_WORKID".to_string(), item.workid.clone()));
    // A user stop kills the session mid-turn (runner polls this path).
    session.stop_flag = Some(stopitem_path(&shared.project_root, &item.workid));
    // Stale flags from a killed prior attempt. (Stale STOP flags are removed
    // at claim time in pick_next, under the record lock — doing it here would
    // race a stop the user sent the moment the item showed in-progress.)
    let _ = std::fs::remove_file(critfail_path(&shared.project_root, &item.workid));
    let _ = std::fs::remove_file(reject_path(&shared.project_root, &item.workid));
    let mut outputs: Vec<String> = Vec::new();

    // Output of engine-run shell steps, carried into the NEXT LLM turn's prompt.
    let mut pending_shell = String::new();
    for (i, step) in turns.iter().enumerate() {
        if shared.stop_now.load(Ordering::SeqCst) {
            logging::warn(&tag, "engine stopping: requeueing after current turn");
            requeue_with_output(&shared, &item.workid, "engine stopped mid-run; requeued", false, Some(outputs.join("\n")));
            drop(lock);
            return;
        }
        // User stop between turns (the mid-turn case is the runner's kill).
        if take_stopitem(&shared.project_root, &item.workid) {
            logging::warn(&tag, &format!("user stop: halting {} → todo", short(&item.workid)));
            stop_item(&shared, &item.workid, outputs.join("\n"));
            drop(lock);
            return;
        }
        // A `.sh` prepostwork step: the engine runs it directly (no LLM); its
        // output lands in the item output and prefaces the next LLM turn.
        if let Some(cmd) = &step.shell {
            match run_shell_command(
                cmd,
                &codepath,
                &session.envs,
                cfg.engine.shell_timeout_sec,
                session.stop_flag.as_deref(),
            ) {
                Ok(out) => {
                    logging::info(&tag, &format!("{} done (engine-run)", step.turn.label));
                    outputs.push(format!("[{}] $ {}\n{}", step.turn.label, cmd, out));
                    pending_shell.push_str(&format!("\n\n# Output of {} (engine-run shell step)\n{}", step.turn.label, out));
                    stamp_boundaries(&shared, &item.workid, &turns, i);
                    continue;
                }
                Err(e) => {
                    if take_stopitem(&shared.project_root, &item.workid) || e == crate::runner::STOPPED_BY_USER {
                        logging::warn(&tag, &format!("user stop: halting {} → todo", short(&item.workid)));
                        stop_item(&shared, &item.workid, outputs.join("\n"));
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
        let turn = if pending_shell.is_empty() {
            step.turn.clone()
        } else {
            let t = Turn {
                label: step.turn.label.clone(),
                prompt: format!("{}\n\n---\n{}", pending_shell.trim_start(), step.turn.prompt),
            };
            pending_shell.clear();
            t
        };
        let mut turn_result = session.run(&turn);
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
            // `iter reject`: the agent judged the WORK invalid. Back to todo for
            // human re-evaluation — no retries, no close-out. Critfail wins if
            // both flags exist (a failed review is the harder signal).
            if turn_result.is_ok() {
                if let Some(reason) = take_reject(&shared.project_root, &item.workid) {
                    logging::warn(&tag, &format!("agent rejected the work item → todo: {}", reason));
                    reject_item(&shared, &item.workid, &reason, outputs.join("\n"));
                    drop(lock);
                    return;
                }
            }
        }
        match turn_result {
            Ok(_) => {}
            Err(e) => {
                // User stop first: the runner's mid-turn kill surfaces as this
                // error (and the flag may still be on disk — consume it either
                // way). Routed to todo, never the failed/retry path.
                if take_stopitem(&shared.project_root, &item.workid) || e == crate::runner::STOPPED_BY_USER {
                    logging::warn(&tag, &format!("user stop: halting {} → todo", short(&item.workid)));
                    stop_item(&shared, &item.workid, outputs.join("\n"));
                    drop(lock);
                    return;
                }
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

    // A stop that lands after the final turn finished changed nothing —
    // consume the flag so it cannot ambush a later run.
    if take_stopitem(&shared.project_root, &item.workid) {
        logging::info(&tag, "user stop arrived after the final turn; item completed anyway");
    }
    close_complete(&shared, &item, outputs.join("\n"), &tag);
    drop(lock);
}

/// Close an item out as complete (shared by agent and shell runs):
/// remove from the open queue, append to workitems_closed.jsonl.
fn close_complete(shared: &Shared, item: &WorkItem, output: String, tag: &str) {
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
        logging::error(tag, &format!("close-out failed: {}", e));
    }
    logging::info(tag, &format!("complete → workitems_closed.jsonl; lock released ({})", short(&item.workid)));
}

/// exec:"shell" items: the engine runs prework lines, mainwork, then postwork
/// lines as `sh -c` commands in the item's codepath — no agent, no LLM, no
/// spend. Same lifecycle as agent runs otherwise: codepath lock, attempts and
/// backoff on failure, close-out on success; each command's stdout+stderr is
/// captured into the item's output.
fn run_shell_workitem(shared: Arc<Shared>, item: WorkItem, tag: String) {
    let cfg = shared.cfg();
    let code_root = config::code_root(&shared.project_root, &cfg);
    let codepath = resolve_codepath(&code_root, &item.codepath);
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
    let lock = match locks::acquire_codepath_lock(
        &codepath,
        &item.workid,
        "shell",
        cfg.engine.codepath_lock_timeout_sec,
        &item.codepath_ignore,
    ) {
        Ok(lock) => lock,
        Err(conflict) => {
            let backoff = cfg.engine.codepath_conflict_backoff_sec;
            logging::info(
                &tag,
                &format!("codepath busy ({}); requeued {} (retry in {}s)", conflict.display(), short(&item.workid), backoff),
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

    // Same env contract agents get, so `"$ITER_BIN" runtests …` etc. just work.
    let iter_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "iter".to_string());
    let reqs_dir = crate::context::reqs_dir(&shared.project_root, &code_root);
    let envs: Vec<(String, String)> = vec![
        ("ITER_BIN".into(), iter_bin),
        ("ITER_PROJECT".into(), shared.project_root.to_string_lossy().into_owned()),
        ("ITER_REQS".into(), reqs_dir.to_string_lossy().into_owned()),
        ("ITER_TEST_DIR".into(), cfg.globalsettings.test_dir.clone()),
        ("ITER_INTERFACE_DIR".into(), config::interface_dir(&shared.project_root, &cfg).to_string_lossy().into_owned()),
        ("ITER_USECASE_DIR".into(), config::usecase_dir(&shared.project_root, &cfg).to_string_lossy().into_owned()),
        ("ITER_WORKID".into(), item.workid.clone()),
    ];

    // Undo point, mirroring the agent path (stale stop flags already died at
    // claim time in pick_next, under the record lock).
    record_git_baseline(&shared, &item.workid, &codepath);
    let stop_flag = stopitem_path(&shared.project_root, &item.workid);

    let mut steps: Vec<(String, String)> = Vec::new();
    for (i, p) in item.prework.iter().enumerate() {
        steps.push((format!("prework[{}]", i + 1), p.clone()));
    }
    steps.push(("mainwork".into(), item.mainwork.clone()));
    for (i, p) in item.postwork.iter().enumerate() {
        steps.push((format!("postwork[{}]", i + 1), p.clone()));
    }
    let mut outputs: Vec<String> = Vec::new();
    for (label, cmd) in &steps {
        if shared.stop_now.load(Ordering::SeqCst) {
            logging::warn(&tag, "engine stopping: requeueing shell item");
            requeue_with_output(&shared, &item.workid, "engine stopped mid-run; requeued", false, Some(outputs.join("\n")));
            drop(lock);
            return;
        }
        if take_stopitem(&shared.project_root, &item.workid) {
            logging::warn(&tag, &format!("user stop: halting {} → todo", short(&item.workid)));
            stop_item(&shared, &item.workid, outputs.join("\n"));
            drop(lock);
            return;
        }
        match run_shell_command(cmd, &codepath, &envs, cfg.engine.shell_timeout_sec, Some(&stop_flag)) {
            Ok(out) => {
                logging::info(&tag, &format!("{} done", label));
                outputs.push(format!("[{}] $ {}\n{}", label, cmd, out));
                if label == "mainwork" {
                    let _q = shared.queue_mutex.lock().unwrap();
                    let now = workitems::now_iso();
                    let _ = shared.queue().mutate(&item.workid, |it| it.times.mainworkdone = now.clone());
                }
            }
            Err(e) => {
                if take_stopitem(&shared.project_root, &item.workid) || e == crate::runner::STOPPED_BY_USER {
                    logging::warn(&tag, &format!("user stop: halting {} → todo", short(&item.workid)));
                    stop_item(&shared, &item.workid, outputs.join("\n"));
                    drop(lock);
                    return;
                }
                let msg = format!("{} failed: {}", label, e);
                logging::error(&tag, &msg);
                fail_item(&shared, &item, &msg, outputs.join("\n"), &tag);
                drop(lock);
                return;
            }
        }
    }
    if take_stopitem(&shared.project_root, &item.workid) {
        logging::info(&tag, "user stop arrived after the final step; item completed anyway");
    }
    close_complete(&shared, &item, outputs.join("\n"), &tag);
    drop(lock);
}

/// Longest stdout+stderr tail a shell step keeps; protects workitems.jsonl
/// from a chatty command.
const SHELL_OUTPUT_TAIL_CHARS: usize = 65536;

fn run_shell_command(
    cmd: &str,
    cwd: &Path,
    envs: &[(String, String)],
    timeout_sec: u64,
    stop_flag: Option<&Path>,
) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    // Drain pipes on threads so a chatty command can't deadlock on a full pipe
    // while we poll for exit.
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_h = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let err_h = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(timeout_sec.max(1));
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                // User stop: kill the command mid-run, same as an agent turn.
                if stop_flag.is_some_and(|f| f.exists()) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(crate::runner::STOPPED_BY_USER.to_string());
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s (shell_timeout_sec)", timeout_sec));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(format!("wait failed: {}", e)),
        }
    };
    let mut text = out_h.join().unwrap_or_default();
    let err_text = err_h.join().unwrap_or_default();
    if !err_text.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&err_text);
    }
    if text.len() > SHELL_OUTPUT_TAIL_CHARS {
        let cut = text.len() - SHELL_OUTPUT_TAIL_CHARS;
        let safe = (cut..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
        text = format!("… ({} chars trimmed)\n{}", safe, &text[safe..]);
    }
    if status.success() {
        Ok(text)
    } else {
        let tail: String = text.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
        Err(format!("exit {}\n{}", status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into()), tail))
    }
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

/// HEAD of the item's codepath repo at run start, recorded on the item as
/// `git_start_commit` — the undo point the webapp offers when a run is stopped
/// mid-stream (`git reset --hard <sha>`). Left untouched when the codepath is
/// not inside a git repo (no commit prior to starting → no undo hint).
fn record_git_baseline(shared: &Shared, workid: &str, codepath: &Path) {
    let out = std::process::Command::new("git")
        .args(["-C", &codepath.to_string_lossy(), "rev-parse", "HEAD"])
        .output();
    let Ok(out) = out else { return };
    if !out.status.success() {
        return;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return;
    }
    let _q = shared.queue_mutex.lock().unwrap();
    let _ = shared.queue().mutate(workid, |it| it.git_start_commit = sha.clone());
}

/// User-stop outcome: like `iter reject`, the item returns to `todo` — the
/// stopped work was judged errantly started, so a human re-evaluates; retries
/// would just restart what the user halted. Turns completed so far are kept in
/// output; the note warns the work may be partial.
fn stop_item(shared: &Shared, workid: &str, partial_output: String) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| {
        it.state = workitems::STATE_TODO.into();
        it.lasterror = "STOPPED by user mid-run — work may be partially completed; \
                        git_start_commit is the undo point"
            .into();
        if !partial_output.is_empty() {
            it.output = partial_output.clone();
        }
        it.times.start = String::new();
    });
}

/// `iter reject` outcome: the item returns to `todo` (the human-review bucket —
/// deliberately NOT complete, which is too big a pile to surface rejections
/// from, and NOT failed, which would burn retries re-deriving the same
/// rejection). The reason lands in lasterror; turns completed so far are kept
/// in output so the re-evaluating human sees the agent's analysis.
fn reject_item(shared: &Shared, workid: &str, reason: &str, partial_output: String) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| {
        it.state = workitems::STATE_TODO.into();
        it.lasterror = format!("REJECTED by agent: {}", reason);
        if !partial_output.is_empty() {
            it.output = partial_output.clone();
        }
        it.times.start = String::new();
    });
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
        let (label, prompt, shell) = resolve_prepost(&shared.project_root, entry, "prework");
        steps.push(StepTurn { phase: Phase::Prework, turn: Turn { label, prompt }, shell });
    }
    steps.push(StepTurn {
        phase: Phase::Mainwork,
        turn: Turn { label: "mainwork".into(), prompt: item.mainwork.clone() },
        shell: None,
    });
    for entry in &item.postwork {
        let (label, prompt, shell) = resolve_prepost(&shared.project_root, entry, "postwork");
        steps.push(StepTurn { phase: Phase::Postwork, turn: Turn { label, prompt }, shell });
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
        shell: None,
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
    // Spin-up goes on the first LLM turn — never into a shell step's command.
    if let Some(first) = steps.iter_mut().find(|s| s.shell.is_none()) {
        first.turn.prompt = format!("{}\n\n# Step: {}\n{}", spinup, first.turn.label, first.turn.prompt);
    }
    steps
}

/// Prepostwork resolution rule: the entry is a filename minus extension. If
/// `.iter/prepostwork/<entry>.sh` exists this step is an engine-run SHELL
/// command (third element); if `.iter/prepostwork/<entry>.md` exists its
/// content is the prompt; otherwise the entry itself is a literal inline prompt.
fn resolve_prepost(project_root: &Path, entry: &str, phase: &str) -> (String, String, Option<String>) {
    let dir = project_root.join(".iter").join("prepostwork");
    let sh = dir.join(format!("{}.sh", entry));
    if sh.is_file() {
        return (
            format!("{}:{} (shell)", phase, entry),
            String::new(),
            Some(format!("sh '{}'", sh.display())),
        );
    }
    let file = dir.join(format!("{}.md", entry));
    match std::fs::read_to_string(&file) {
        Ok(content) => (format!("{}:{}", phase, entry), content, None),
        Err(_) => {
            let short: String = entry.chars().take(30).collect();
            (format!("{}:inline({}…)", phase, short.trim_end()), entry.to_string(), None)
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
        assert!(pick_next(&shared, &["code"], false).is_none(), "deferred item must not be re-picked");

        // ...and picked again once the backoff expires.
        shared.deferred.lock().unwrap().insert("w1".into(), Instant::now() - std::time::Duration::from_secs(1));
        let picked = pick_next(&shared, &["code"], false).expect("expired deferral must be pickable");
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
        // Most urgent (lowest P) overlaps a running item's scope; next is under an
        // on-disk lock; the third must be picked even though it sorts last.
        shared.queue().append(&mk("w-occupied", 1, "a/deep")).unwrap();
        shared.queue().append(&mk("w-locked", 2, "b")).unwrap();
        shared.queue().append(&mk("w-free", 9, "c")).unwrap();
        shared
            .running_paths
            .lock()
            .unwrap()
            .insert("running".into(), (root.join("a").canonicalize().unwrap(), Vec::new()));
        let _lock = locks::acquire_codepath_lock(&root.join("b"), "other-engine", "code", 600, &[]).unwrap();
        let picked = pick_next(&shared, &["code"], false).expect("must keep moving down the queue");
        assert_eq!(picked.workid, "w-free");
        // Nothing else runnable now.
        assert!(pick_next(&shared, &["code"], false).is_none());
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
            priority: 1,
            codepath: "obj/src".into(),
            ..Default::default()
        };
        shared.queue().append(&clash).unwrap();
        shared.queue().append(&tw).unwrap();
        let picked = pick_next(&shared, &["testwriter"], false).expect("ignored subtree must be pickable");
        assert_eq!(picked.workid, "w-tw");
        assert!(pick_next(&shared, &["testwriter"], false).is_none(), "non-ignored sibling stays blocked");

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
            priority: 1,
            codepath: "obj".into(),
            ..Default::default()
        };
        shared.queue().append(&code_full).unwrap();
        shared.queue().append(&code_ign).unwrap();
        let picked = pick_next(&shared, &["code"], false).expect("guard-repaired item must be pickable");
        assert_eq!(picked.workid, "w-code-full", "more urgent priority wins once the guard repairs its scope");
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

    // The dependency dispatch gate (workitem_dependency.md): a queued item with
    // an unsatisfied dependency is never dispatched, however urgent.
    #[test]
    fn dependency_gate_blocks_dispatch() {
        let root = std::env::temp_dir().join(format!("iterloop-depgate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["a", "b"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let shared = test_shared(&root);
        let a = WorkItem {
            workid: "dep-a".into(),
            item_type: "code".into(),
            state: workitems::STATE_IN_PROGRESS.into(),
            codepath: "a".into(),
            ..Default::default()
        };
        // B is MORE urgent (P0) and on a disjoint codepath — only the gate
        // holds it back; break the gate and this test goes red.
        let b = WorkItem {
            workid: "dep-b".into(),
            item_type: "code".into(),
            priority: 0,
            codepath: "b".into(),
            depends_on: vec!["dep-a".into()],
            ..Default::default()
        };
        shared.queue().append(&a).unwrap();
        shared.queue().append(&b).unwrap();
        assert!(pick_next(&shared, &["code"], false).is_none(), "B must not dispatch while its dependency is open");

        // A closes complete (no descendants) → B dispatches.
        let mut done = a.clone();
        done.state = workitems::STATE_COMPLETE.into();
        shared.queue().close(&done).unwrap();
        let picked = pick_next(&shared, &["code"], false).expect("satisfied dependency releases the item");
        assert_eq!(picked.workid, "dep-b");
        let _ = std::fs::remove_dir_all(&root);
    }

    // A FAILED dependency flips the dependent to `todo` with the blocker named
    // — never a silent hang, never a run on a broken foundation. A `todo` item
    // with dependencies is untouched (state semantics pinned).
    #[test]
    fn failed_dependency_flips_queued_to_todo() {
        let root = std::env::temp_dir().join(format!("iterloop-depfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("b")).unwrap();
        let shared = test_shared(&root);
        let queue = shared.queue();
        let a_failed = WorkItem { workid: "dep-a".into(), state: workitems::STATE_FAILED.into(), ..Default::default() };
        queue.append_closed(&a_failed).unwrap();
        let b = WorkItem {
            workid: "dep-b".into(),
            item_type: "code".into(),
            codepath: "b".into(),
            depends_on: vec!["dep-a".into()],
            ..Default::default()
        };
        let parked = WorkItem {
            workid: "dep-c".into(),
            item_type: "code".into(),
            state: workitems::STATE_TODO.into(),
            depends_on: vec!["dep-a".into()],
            ..Default::default()
        };
        queue.append(&b).unwrap();
        queue.append(&parked).unwrap();

        let flips = flip_failed_dependents(&queue);
        assert_eq!(flips.len(), 1, "only the QUEUED dependent flips: {:?}", flips);
        assert_eq!(flips[0].0, "dep-b");
        let items = queue.load();
        let b_now = items.iter().find(|i| i.workid == "dep-b").unwrap();
        assert_eq!(b_now.state, workitems::STATE_TODO);
        assert!(
            b_now.lasterror.contains("DEPENDENCY FAILED") && b_now.lasterror.contains("dep-a"),
            "note names the failed dependency: {}",
            b_now.lasterror
        );
        let c_now = items.iter().find(|i| i.workid == "dep-c").unwrap();
        assert_eq!(c_now.state, workitems::STATE_TODO);
        assert!(c_now.lasterror.is_empty(), "todo items are untouched until queued");
        // And nothing dispatches.
        assert!(pick_next(&shared, &["code"], false).is_none());
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
        shared.queue().append(&mk("w-refactor", "refactor", 0, "b")).unwrap();
        shared.queue().append(&mk("w-ingest", "ingest", 4, "c")).unwrap();
        // P0 wins even though "code" sorts first alphabetically.
        let types = ["code", "ingest", "refactor"];
        assert_eq!(pick_next(&shared, &types, false).unwrap().workid, "w-refactor");
        assert_eq!(pick_next(&shared, &types, false).unwrap().workid, "w-ingest");
        // With refactor's slot no longer offered, only code remains.
        assert_eq!(pick_next(&shared, &["code", "ingest"], false).unwrap().workid, "w-code");
        assert!(pick_next(&shared, &types, false).is_none());
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
        let (label, prompt, shell) = resolve_prepost(&root, "git-pull", "prework");
        assert!(shell.is_none());
        assert_eq!(label, "prework:git-pull");
        assert!(prompt.contains("git pull --rebase"));

        let (label, prompt, _) = resolve_prepost(&root, "Just do this literal thing", "prework");
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
