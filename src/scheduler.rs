use serde::{Deserialize, Serialize};
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
    /// CONSECUTIVE codepath-lock conflicts per workid, driving the escalating
    /// backoff in `defer_after_conflict`. Reset the moment the item gets its
    /// lock; pruned in pick_next when the item leaves the open queue.
    conflicts: Mutex<HashMap<String, u32>>,
    /// Why the last pick pass SKIPPED each queued item it could not run:
    /// workid → the lock scope covering it and who holds it. The picker used to
    /// skip silently, so a queue full of runnable-looking work with one agent on
    /// it had no visible explanation. Published to `.iter/.engine/blocked.json`
    /// each tick for the webapp, and summarized into the log when it changes.
    blocked: Mutex<HashMap<String, Blocked>>,
    /// Resolved lock scopes of items currently running in THIS engine
    /// (workid → [(path, codepath_ignore)] — structureV2: an item may carry
    /// several codepaths and ALL are claimed). pick_next skips candidates that
    /// overlap one, so an occupied lock scope never costs a pick; entries are
    /// removed when the worker thread finishes.
    running_paths: Mutex<HashMap<String, Vec<(PathBuf, Vec<String>)>>>,
    /// Usage-limit hold (limits::Hold): pick nothing until the window reopens.
    /// Set by a worker whose turn came back 429; the engine stays alive and
    /// auto-resumes at the reset the API named. Concurrent workers hitting the
    /// same closed window fold into ONE hold — see `enter_limit_hold`.
    limit_hold: Mutex<Option<limits::Hold>>,
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

    /// Close the picking window on a 429. Answers whether THIS caller is the one
    /// that closed it, and when the window now reopens. Several agents are
    /// normally in flight, so a window that shuts announces itself to every one
    /// of them within a second or two; only the first gets to say so, which is
    /// what turns hours of per-spawn log spam into one line. When a hold is
    /// already standing the later reset wins — a second error naming a
    /// further-out window extends the hold, never shortens it.
    fn enter_limit_hold(
        &self,
        hold: limits::Hold,
        now: chrono::DateTime<chrono::Utc>,
    ) -> (bool, chrono::DateTime<chrono::Utc>) {
        let mut cur = self.limit_hold.lock().unwrap();
        match cur.as_mut() {
            Some(standing) if standing.until > now => {
                if hold.until > standing.until {
                    standing.until = hold.until;
                    standing.authoritative = hold.authoritative;
                }
                (false, standing.until)
            }
            _ => {
                let until = hold.until;
                *cur = Some(hold);
                (true, until)
            }
        }
    }
}

pub fn stop_signal_path(project_root: &Path) -> PathBuf {
    config::engine_dir(project_root).join("stop.signal")
}

/// One queued item's lock gate, as the picker saw it on its last pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Blocked {
    /// The workid holding the covering scope (empty if the lock file is
    /// unreadable, or belongs to an engine that is gone).
    pub by: String,
    /// The lock scope that covers this item's codepath.
    pub path: String,
    /// "running" — an item live in THIS engine; "lock" — an `.iter.lock` on
    /// disk (another engine, or a leftover from a crash).
    pub kind: String,
}

/// Where the picker publishes its skip reasons for the webapp to read.
pub fn blocked_path(project_root: &Path) -> PathBuf {
    config::engine_dir(project_root).join("blocked.json")
}

/// The lock gates recorded by the last pick pass. Empty when the engine is not
/// picking (stopped, draining, or on a usage hold) — the file is cleared then,
/// so a stale map can never make a runnable queue look blocked.
pub fn read_blocked(project_root: &Path) -> HashMap<String, Blocked> {
    std::fs::read_to_string(blocked_path(project_root))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
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

/// Question-flag written by `iter ask` (features/Question_state.md): the agent
/// hit a decision only a human can make. Same turn-boundary consumption as the
/// reject flag, but the item lands in `question` with the text stored in its
/// `question` field — parked until a person answers, then queued.
pub fn question_path(project_root: &Path, workid: &str) -> PathBuf {
    config::engine_dir(project_root).join(format!("question-{}.txt", workid))
}

fn take_question(project_root: &Path, workid: &str) -> Option<String> {
    let path = question_path(project_root, workid);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
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
        conflicts: Mutex::new(HashMap::new()),
        blocked: Mutex::new(HashMap::new()),
        running_paths: Mutex::new(HashMap::new()),
        limit_hold: Mutex::new(None),
    });

    // Startup crash recovery — both halves: the items go back to `queued`, AND
    // the codepath locks their dead engine abandoned are released. Leaving those
    // behind used to block their own scope (and everything under it) until the
    // lock timeout expired, so a restart mid-run could serialize the engine for
    // an hour with no visible cause.
    {
        let _q = shared.queue_mutex.lock().unwrap();
        match workitems::recover_orphans(&shared.queue()) {
            Ok(recovered) if recovered.is_empty() => {}
            Ok(recovered) => {
                logging::warn(
                    "engine",
                    &format!("recovered {} orphaned in-progress item(s) back to queued", recovered.len()),
                );
                let code_root = config::code_root(&project_root, &shared.cfg());
                for (path, workid) in locks::release_orphaned(&code_root, &recovered) {
                    logging::warn(
                        "engine",
                        &format!(
                            "released the codepath lock {} abandoned by orphaned item {} (its engine is gone)",
                            path.display(),
                            short(&workid)
                        ),
                    );
                }
            }
            Err(e) => logging::error("engine", &format!("orphan recovery failed: {}", e)),
        }
    }

    let mut running: Vec<(String, JoinHandle<()>)> = Vec::new();
    let mut worker_seq: u64 = 0;
    let mut stop_picking = false;
    let mut draining = false;
    let mut tick: u64 = 0;
    let mut last_summary = String::new();
    let mut last_summary_at = Instant::now();
    let mut last_blocked: HashMap<String, Blocked> = HashMap::new();
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
        let effective_max = tier.map_or(cfg.limits.max_total_agents, |c| c.min(cfg.limits.max_total_agents));
        // The cap line carries its own inputs. Two of them are not obvious from
        // the webapp header and made this line look non-deterministic in the
        // field: the percentage that picks the band is the MAX of the two
        // windows (each counted as 0 once its reset time has passed), not the
        // 5h figure alone; and config.json is re-read every tick, so editing
        // max_agents_at_NN moves the cap live at an unchanged percentage.
        if last_tier_cap != Some(tier) {
            match (tier, pct) {
                (Some(cap), Some(p)) => {
                    let band = if p >= 95.0 { 95 } else if p >= 90.0 { 90 } else { 80 };
                    let (h5, d7) = usage
                        .as_ref()
                        .map(|u| (u.five_hour_pct, u.seven_day_pct))
                        .unwrap_or((0.0, 0.0));
                    logging::warn(
                        "engine",
                        &format!(
                            "account usage {:.0}% (5h {:.0}%, 7d {:.0}% — the throttle uses the higher, \
                             reset windows counted as 0) → {}% band → max_agents_at_{} = {}; \
                             effective max agents {} (max_total_agents {})",
                            p, h5, d7, band, band, cap, effective_max, cfg.limits.max_total_agents
                        ),
                    )
                }
                (None, Some(p)) => logging::info(
                    "engine",
                    &format!("account usage {:.0}% — below the 80% band, tier throttle off", p),
                ),
                _ => {}
            }
            last_tier_cap = Some(tier);
        }

        // Usage-limit hold: set by a worker whose turn came back 429. It ends at
        // the reset the API named — the statusline snapshot gets no vote there
        // (limits::Hold documents why), and only a GUESSED hold can be cut short
        // by data newer than the error itself. Both log lines a hold produces
        // are one-per-hold: the close is announced by the worker that hit it,
        // the reopen right here.
        let mut holding = false;
        {
            let mut hold = shared.limit_hold.lock().unwrap();
            if let Some(h) = hold.as_ref() {
                let early = h.may_lift_early(usage.as_ref(), now_utc);
                if now_utc >= h.until || early {
                    logging::info(
                        "engine",
                        if early {
                            "usage window reopened early (fresh snapshot, no stated reset); resuming picking"
                        } else {
                            "usage window reopened; resuming picking"
                        },
                    );
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
                // Claim every resolved codepath before spawning, so a second
                // pick in this same tick already sees them as occupied.
                let base = config::code_root(&shared.project_root, &cfg);
                let resolved: Vec<(PathBuf, Vec<String>)> = item
                    .all_codepaths()
                    .iter()
                    .map(|p| (resolve_codepath(&base, p), item.codepath_ignore.clone()))
                    .collect();
                shared.running_paths.lock().unwrap().insert(item.workid.clone(), resolved);
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
        // Publish the picker's skip reasons (features: lock visibility). When the
        // engine is not picking at all, the map is cleared — a stale one would
        // make a perfectly runnable queue look lock-blocked.
        {
            if stop_picking || holding {
                shared.blocked.lock().unwrap().clear();
            }
            let snapshot = shared.blocked.lock().unwrap().clone();
            if snapshot != last_blocked {
                let path = blocked_path(&project_root);
                if snapshot.is_empty() {
                    let _ = std::fs::remove_file(&path);
                } else if let Ok(text) = serde_json::to_string_pretty(&snapshot) {
                    let _ = std::fs::write(&path, text);
                }
                // The log line answers "why is only one thing running": how many
                // slots are free, how many items are waiting, and on whom.
                if !snapshot.is_empty() {
                    let mut holders: Vec<(&str, usize)> = Vec::new();
                    for b in snapshot.values() {
                        let key = if b.by.is_empty() { b.path.as_str() } else { b.by.as_str() };
                        match holders.iter_mut().find(|(k, _)| *k == key) {
                            Some((_, n)) => *n += 1,
                            None => holders.push((key, 1)),
                        }
                    }
                    holders.sort_by(|a, b| b.1.cmp(&a.1));
                    let named: Vec<String> = holders
                        .iter()
                        .take(3)
                        .map(|(k, n)| format!("{} blocks {}", short(k), n))
                        .collect();
                    logging::info(
                        "engine",
                        &format!(
                            "{} free agent slot(s), but {} queued item(s) sit inside a locked scope ({})",
                            effective_max.saturating_sub(running.len()),
                            snapshot.len(),
                            named.join(", ")
                        ),
                    );
                }
                last_blocked = snapshot;
            }
        }

        // Logged when it changes — plus a heartbeat at a fixed interval even when
        // nothing has, so tick numbers advance in the log at least this often. A
        // silent stream otherwise reads the same whether the queue is quiet or
        // the loop has died, and the gaps in tick numbering look like lost ticks.
        let summary = summarize(&items, running.len());
        let beat = last_summary_at.elapsed().as_secs() >= HEARTBEAT_SEC;
        if summary != last_summary || beat {
            logging::info(
                "engine",
                &format!("tick #{} — {}{}", tick, summary, if beat && summary == last_summary { " (heartbeat)" } else { "" }),
            );
            last_summary = summary;
            last_summary_at = Instant::now();
        }

        // Usage probe + staleness: only relevant while there is work to run (or a
        // hold to lift). The probe pokes a background interactive claude session so
        // its statusline refreshes the snapshot; idle engines never poke.
        let work_present =
            !running.is_empty() || holding || items.iter().any(|i| i.eligible(&cfg, now_utc));
        if work_present && !stop_picking {
            let stale_sec = usage.as_ref().map(|u| u.age_sec(now_utc)).unwrap_or(i64::MAX);
            if stale_sec > limits::SNAPSHOT_STALE_WARN_SEC as i64 {
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
                && stale_sec > limits::PROBE_INTERVAL_SEC as i64
                && probe_last.elapsed().as_secs() >= limits::PROBE_INTERVAL_SEC
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
                    it.todo_reason = workitems::TODO_REASON_GUARD.into();
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
        "queue: {} open ({} queued, {} in-progress, {} paused, {} failed, {} todo, {} question); {} agent(s) running",
        items.len(),
        count(workitems::STATE_QUEUED),
        count(workitems::STATE_IN_PROGRESS),
        count(workitems::STATE_PAUSED),
        count(workitems::STATE_FAILED),
        count(workitems::STATE_TODO),
        count(workitems::STATE_QUESTION),
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
    // Conflict streaks belong to items that still exist; a closed or deleted
    // workid must not carry its backoff history into a clone of the same id.
    {
        let open: std::collections::HashSet<&str> = items.iter().map(|i| i.workid.as_str()).collect();
        shared.conflicts.lock().unwrap().retain(|id, _| open.contains(id.as_str()));
    }
    let now = chrono::Utc::now();
    let code_root = config::code_root(&shared.project_root, &cfg);
    // Scopes held by items live in THIS engine, plus which item holds each —
    // the owner is what makes a "blocked by" message name a work item rather
    // than just a directory.
    let (occupied, running_owner): (Vec<(PathBuf, Vec<String>)>, HashMap<PathBuf, String>) = {
        let map = shared.running_paths.lock().unwrap();
        let mut scopes = Vec::new();
        let mut owner = HashMap::new();
        for (workid, paths) in map.iter() {
            for (p, ign) in paths {
                scopes.push((p.clone(), ign.clone()));
                owner.insert(p.clone(), workid.clone());
            }
        }
        (scopes, owner)
    };
    let mut blocked: HashMap<String, Blocked> = HashMap::new();
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
        // structureV2: EVERY codepath the item carries must be free. A candidate
        // that cannot run is skipped — but the reason is RECORDED, so "nine
        // queued items and one agent" has a visible cause instead of looking
        // like the engine ignoring work.
        let cand_paths: Vec<PathBuf> =
            item.all_codepaths().iter().map(|p| resolve_codepath(&code_root, p)).collect();
        let gate = cand_paths.iter().find_map(|cp| {
            if let Some((holder, r)) = occupied
                .iter()
                .find(|(r, r_ign)| scopes_overlap(r, r_ign, cp, &effective_ignore))
                .map(|(r, _)| (running_owner.get(r).cloned().unwrap_or_default(), r))
            {
                return Some(Blocked {
                    by: holder,
                    path: r.to_string_lossy().into_owned(),
                    kind: "running".into(),
                });
            }
            locks::find_ancestor_lock_info(cp, now).map(|(lock_file, info)| Blocked {
                by: info.workid,
                path: lock_file.parent().unwrap_or(&lock_file).to_string_lossy().into_owned(),
                kind: "lock".into(),
            })
        });
        if let Some(b) = gate {
            blocked.insert(item.workid.clone(), b);
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
    // Publish before the early return: a pass that finds nothing to run is
    // exactly the pass whose reasons a human wants to see.
    *shared.blocked.lock().unwrap() = blocked;
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
        // Whatever parked this item in `todo` before (a guard, a broken
        // codepath) has been dealt with by whoever queued it again — carrying
        // the reason into the run would leave a stale "broken configuration"
        // chip on an item that is visibly working.
        item.todo_reason.clear();
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

/// EVERY codepath an item carries must resolve to a real DIRECTORY before the
/// run starts — not just the primary one. A bad secondary entry used to skip
/// this check and fall through to the lock scan, where it surfaced as "codepath
/// busy": a path that does not exist can never become un-busy, so the item
/// requeued (with its attempt refunded) every backoff forever, indistinguishable
/// in the log from ordinary contention.
///
/// A path that exists but is a FILE fails here too. A codepath is a lock scope,
/// and the engine takes that lock by writing `<path>/.iter.lock` — only a
/// directory can hold one. Worse, a file's ancestor chain is scanned for locks,
/// so a one-file scope inherits every conflict of the directory holding it.
fn validate_codepaths(code_root: &Path, item: &WorkItem) -> Result<(), String> {
    for stored in item.all_codepaths() {
        let path = resolve_codepath(code_root, &stored);
        if path.is_dir() {
            continue;
        }
        return Err(if path.exists() {
            format!(
                "codepath is a file, not a directory: {} (stored \"{}\", resolved against code_root {}) \
                 — a codepath is the lock scope the agent owns, taken by writing <path>/.iter.lock, \
                 which only a directory can hold; name the directory to edit and put the file in mainwork",
                path.display(),
                stored,
                code_root.display()
            )
        } else {
            format!(
                "codepath does not exist: {} (stored \"{}\", resolved against code_root {})",
                path.display(),
                stored,
                code_root.display()
            )
        });
    }
    Ok(())
}

/// Creation-time guard shared by `iter add` and the webapp API: a codepath that
/// EXISTS and is a file is wrong the moment it is written, so refuse it there
/// rather than letting it fail hours later at dispatch. Deliberately narrower
/// than `validate_codepaths` — a path that does not exist YET is allowed,
/// because a plan routinely creates work for a directory an earlier item makes.
pub fn reject_file_codepath(code_root: &Path, item: &WorkItem) -> Result<(), String> {
    for stored in item.all_codepaths() {
        let path = resolve_codepath(code_root, &stored);
        if path.is_file() {
            return Err(format!(
                "codepath \"{}\" is a file ({}). A codepath is the directory tree the item locks and \
                 may edit — the engine takes that lock by writing <path>/.iter.lock, which only a \
                 directory can hold, and a file scope silently inherits every lock conflict of the \
                 directory containing it. Use the directory, and name the file in mainwork.",
                stored,
                path.display()
            ));
        }
    }
    Ok(())
}

/// An item whose lock scope IS the code root owns the entire tree: nothing else
/// can run while it does, whatever the agent caps say. That is occasionally
/// exactly right (a cross-repo verification pass), so this WARNS and never
/// refuses — the cost is just invisible otherwise, and it is the single most
/// common reason a full queue runs one item at a time.
pub fn whole_tree_warning(code_root: &Path, item: &WorkItem) -> Option<String> {
    let root = code_root.canonicalize().unwrap_or_else(|_| code_root.to_path_buf());
    let hit = item.all_codepaths().into_iter().find(|p| resolve_codepath(code_root, p) == root)?;
    // `**` carves out everything: the item takes no lock at all, which is the
    // right shape for a pass that only reads.
    if locks::ignored(Path::new("anything"), &item.codepath_ignore) {
        return None;
    }
    Some(format!(
        "codepath \"{}\" resolves to the code root ({}), so this item LOCKS THE WHOLE TREE — \
         no other work item can run while it does. If it only reads across the tree, add \
         codepath_ignore [\"**\"] to make it lockless; if it writes in one place, scope it there.",
        hit,
        root.display()
    ))
}

/// Ceiling on the escalating lock-conflict backoff: a genuinely long-lived
/// conflict rechecks twice an hour instead of four times a minute.
const MAX_CONFLICT_BACKOFF_SEC: u64 = 1800;

/// How often the tick summary is logged even when nothing changed, so a quiet
/// log still proves the loop is alive.
const HEARTBEAT_SEC: u64 = 300;

/// A codepath-lock conflict, with ESCALATING backoff. A flat retry meant a
/// persistently blocked item came back to the picker every 15s forever, and
/// because the requeue preserves priority it was re-picked ahead of work that
/// could actually run. Each consecutive conflict on the same item doubles its
/// wait (capped), and a streak is called out in the log so a stuck item reads
/// differently from ordinary contention. The streak resets the moment the item
/// gets its lock.
fn defer_after_conflict(shared: &Shared, item: &WorkItem, conflict: &Path, tag: &str) {
    let base = shared.cfg().engine.codepath_conflict_backoff_sec.max(1);
    let streak = {
        let mut map = shared.conflicts.lock().unwrap();
        let n = map.entry(item.workid.clone()).or_insert(0);
        *n += 1;
        *n
    };
    let backoff = base.saturating_mul(1u64 << (streak - 1).min(12)).min(MAX_CONFLICT_BACKOFF_SEC);
    let msg = format!(
        "codepath busy ({}); requeued {} (conflict #{}, retry in {}s)",
        conflict.display(),
        short(&item.workid),
        streak,
        backoff
    );
    if streak >= 5 {
        logging::warn(
            tag,
            &format!("{} — this item has been blocked {} times in a row; check that the blocking lock is real", msg, streak),
        );
    } else {
        logging::info(tag, &msg);
    }
    shared
        .deferred
        .lock()
        .unwrap()
        .insert(item.workid.clone(), Instant::now() + std::time::Duration::from_secs(backoff));
    requeue(shared, &item.workid, "codepath lock conflict", true);
}

/// Acquire the codepath lock on EVERY path an item carries (structureV2: a
/// node's codedirs may put its code in several places, so a fix item locks
/// them all). All-or-nothing: a conflict drops the already-acquired locks
/// (RAII) and returns the conflicting path.
fn acquire_all_codepath_locks(
    code_root: &Path,
    item: &WorkItem,
    agent_type: &str,
    lock_timeout: u64,
) -> Result<Vec<locks::CodepathLock>, PathBuf> {
    let mut acquired = Vec::new();
    for p in item.all_codepaths() {
        let path = resolve_codepath(code_root, &p);
        match locks::acquire_codepath_lock(&path, &item.workid, agent_type, lock_timeout, &item.codepath_ignore) {
            Ok(l) => acquired.push(l),
            Err(conflict) => return Err(conflict), // drops `acquired` → releases
        }
    }
    Ok(acquired)
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

/// Which model this run uses: the item's own `model` when it names one, else the
/// agent type's default (features item 12). The point of the override is that a
/// plan can route its mechanical children — comment sweeps, doc repointing,
/// rename plumbing — to a cheap model without minting a second agent type for
/// them, so the expensive models are spent only where judgment lives.
fn effective_model(agent: &AgentDef, item: &WorkItem) -> String {
    match item.model.trim() {
        "" => agent.model.clone(),
        m => m.to_string(),
    }
}

fn run_workitem(shared: Arc<Shared>, agent: AgentDef, item: WorkItem, tag: String) {
    let cfg = shared.cfg();
    let code_root = config::code_root(&shared.project_root, &cfg);
    let codepath = resolve_codepath(&code_root, &item.codepath);
    let lock_timeout = cfg.engine.codepath_lock_timeout_sec.max(agent.max_work_timeout_sec);

    // A codepath that isn't a real directory is a broken work item, not a busy
    // one — and not a failing one either: no retry can conjure the directory, so
    // it parks for a human instead of grinding through the attempt budget
    // (park_config_error). EVERY entry is checked, not just the primary.
    if let Err(msg) = validate_codepaths(&code_root, &item) {
        logging::error(&tag, &msg);
        park_config_error(&shared, &item, &msg, &tag);
        return;
    }

    // Codepath lock (see .iter/.engine/codepath_lock.md) — structureV2: an
    // item locks EVERY codepath it carries, all-or-nothing.
    let lock = match acquire_all_codepath_locks(&code_root, &item, &agent.type_name, lock_timeout) {
        Ok(lock) => {
            shared.conflicts.lock().unwrap().remove(&item.workid);
            logging::info(&tag, &format!("codepath lock acquired: {}", codepath.display()));
            lock
        }
        Err(conflict) => {
            defer_after_conflict(&shared, &item, &conflict, &tag);
            return;
        }
    };

    // Undo point for a mid-stream user stop.
    record_git_baseline(&shared, &item.workid, &codepath);

    let turns = build_turns(&shared, &agent, &item, &codepath, &tag);
    let mut session = Session::new(agent.clone(), codepath.clone(), shared.project_root.clone());
    // Per-item model override (features item 12). Logged either way, because
    // "which model ran this" is otherwise only recoverable by joining the spend
    // ledger against the agent definition as it stood at the time.
    session.agent.model = effective_model(&agent, &item);
    if session.agent.model == agent.model {
        logging::info(&tag, &format!("model {} ({} default)", agent.model, agent.type_name));
    } else {
        logging::info(
            &tag,
            &format!("model {} (per-item override of the {} default, {})", session.agent.model, agent.type_name, agent.model),
        );
    }
    // The workid lets an `iter critreview` subprocess flag THIS item as failed
    // deterministically (fail-flag file) instead of trusting the agent to stop.
    session.envs.push(("ITER_WORKID".to_string(), item.workid.clone()));
    // $ITER_TEMP (issue 9): agents write scratch files here. Absolute and
    // created up front — the agent instructions used to name a RELATIVE
    // `.iter/temp/`, which resolves against whatever the agent's working
    // directory happens to be and minted a second temp tree at the repo root.
    let temp = config::temp_dir(&shared.project_root, &cfg);
    if let Err(e) = std::fs::create_dir_all(&temp) {
        logging::warn(&tag, &format!("cannot create the temp directory {}: {}", temp.display(), e));
    }
    session.envs.push(("ITER_TEMP".to_string(), temp.to_string_lossy().into_owned()));
    // Every locked codepath, colon-joined (first = the working directory).
    session.envs.push((
        "ITER_CODEPATHS".to_string(),
        item.all_codepaths()
            .iter()
            .map(|p| resolve_codepath(&code_root, p).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
    ));
    // A user stop kills the session mid-turn (runner polls this path).
    session.stop_flag = Some(stopitem_path(&shared.project_root, &item.workid));
    // Stale flags from a killed prior attempt. (Stale STOP flags are removed
    // at claim time in pick_next, under the record lock — doing it here would
    // race a stop the user sent the moment the item showed in-progress.)
    let _ = std::fs::remove_file(critfail_path(&shared.project_root, &item.workid));
    let _ = std::fs::remove_file(reject_path(&shared.project_root, &item.workid));
    let _ = std::fs::remove_file(question_path(&shared.project_root, &item.workid));
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
            // `iter ask`: the agent needs a human decision before it can go on.
            // The item parks in `question`; answering it in the webapp queues it
            // again with the answer in hand.
            if turn_result.is_ok() {
                if let Some(question) = take_question(&shared.project_root, &item.workid) {
                    logging::warn(
                        &tag,
                        &format!("agent asked the human a question → question: {}", first_line(&question)),
                    );
                    ask_item(&shared, &item.workid, &question, outputs.join("\n"));
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
                        let retry = limits::PROBE_INTERVAL_SEC as i64;
                        let (announced, until) =
                            shared.enter_limit_hold(limits::Hold::from_error(&e, now, retry), now);
                        let resuming = limits::local_label(until, &cfg);
                        // One line per closed window, not one per spawn: the
                        // announcing worker carries the whole 429 payload (the
                        // only place it is worth having), every later worker
                        // under the same hold says one short sentence.
                        if announced {
                            logging::error(
                                &tag,
                                &format!("usage window closed, resuming {} — holding all picks until then. The API said: {}", resuming, e),
                            );
                        } else {
                            logging::warn(
                                &tag,
                                &format!("usage window still closed (resuming {}); requeued {}", resuming, short(&item.workid)),
                            );
                        }
                        requeue_with_output(&shared, &item.workid, &format!("usage limit reached; engine holding until {}", resuming), true, Some(outputs.join("\n")));
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
    if let Err(msg) = validate_codepaths(&code_root, &item) {
        logging::error(&tag, &msg);
        park_config_error(&shared, &item, &msg, &tag);
        return;
    }
    let lock = match acquire_all_codepath_locks(&code_root, &item, "shell", cfg.engine.codepath_lock_timeout_sec) {
        Ok(lock) => {
            shared.conflicts.lock().unwrap().remove(&item.workid);
            lock
        }
        Err(conflict) => {
            defer_after_conflict(&shared, &item, &conflict, &tag);
            return;
        }
    };

    // Same env contract agents get, so `"$ITER_BIN" runtests …` etc. just work.
    let iter_bin = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "iter".to_string());
    let temp = config::temp_dir(&shared.project_root, &cfg);
    if let Err(e) = std::fs::create_dir_all(&temp) {
        logging::warn(&tag, &format!("cannot create the temp directory {}: {}", temp.display(), e));
    }
    let envs: Vec<(String, String)> = vec![
        ("ITER_BIN".into(), iter_bin),
        ("ITER_TEMP".into(), temp.to_string_lossy().into_owned()),
        ("ITER_PROJECT".into(), shared.project_root.to_string_lossy().into_owned()),
        (
            "ITER_MAINFILE".into(),
            crate::project::Project::load(&shared.project_root).mainfile.to_string_lossy().into_owned(),
        ),
        (
            "ITER_CONTEXT_FILES".into(),
            config::global_context_files(&shared.project_root)
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":"),
        ),
        ("ITER_TEST_DIR".into(), cfg.globalsettings.test_dir.clone()),
        ("ITER_INTERFACE_DIR".into(), config::interface_dir(&shared.project_root, &cfg).to_string_lossy().into_owned()),
        ("ITER_USECASE_DIR".into(), config::usecase_dir(&shared.project_root, &cfg).to_string_lossy().into_owned()),
        ("ITER_WORKID".into(), item.workid.clone()),
        (
            "ITER_CODEPATHS".into(),
            item.all_codepaths()
                .iter()
                .map(|p| resolve_codepath(&code_root, p).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(":"),
        ),
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
        it.todo_reason = workitems::TODO_REASON_GUARD.into();
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
        it.todo_reason = workitems::TODO_REASON_GUARD.into();
        it.lasterror = format!("REJECTED by agent: {}", reason);
        if !partial_output.is_empty() {
            it.output = partial_output.clone();
        }
        it.times.start = String::new();
    });
}

/// `iter ask` outcome (features/Question_state.md): the item parks in
/// `question` with the agent's text in its `question` field, waiting on a
/// person. Deliberately not `todo` — a question is a human BLOCKING a machine,
/// and the webapp surfaces it as its own bucket. Turns completed so far stay in
/// output (the research behind the question is what makes it answerable), and a
/// stale answer from an earlier round is cleared so the new question reads
/// unambiguously. No retries burned; the attempt is refunded because the item
/// never got to finish its work.
fn ask_item(shared: &Shared, workid: &str, question: &str, partial_output: String) {
    let _q = shared.queue_mutex.lock().unwrap();
    let queue = shared.queue();
    let _ = queue.mutate(workid, |it| {
        it.state = workitems::STATE_QUESTION.into();
        it.question = question.to_string();
        it.answer = String::new();
        it.lasterror = format!("WAITING ON A HUMAN ANSWER: {}", first_line(question));
        if !partial_output.is_empty() {
            it.output = partial_output.clone();
        }
        it.attempts = it.attempts.saturating_sub(1);
        it.times.asked = workitems::now_iso();
        it.times.answered = String::new();
        it.times.start = String::new();
    });
}

/// The first non-empty line of a block of text, trimmed for a log line or an
/// error field — questions are paragraphs, log lines are not.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    if line.chars().count() > 160 {
        format!("{}…", line.chars().take(159).collect::<String>())
    } else {
        line
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

/// A dispatch-validation failure — the item names a codepath that is not a
/// directory the engine can lock — is a CONFIGURATION error, and retrying one is
/// just waiting for a person to notice. Measured on pdy-dev (issue 1,
/// 2026-08-25): one item burned all 50 attempts over five days against a
/// directory that did not exist, milliseconds per attempt, never launching an
/// agent, and the only signal a human got was the item finally closing `failed`
/// with an empty output.
///
/// So this parks instead of failing: `todo` — the human-review bucket — with the
/// attempt refunded (the run never started, so nothing was spent to burn one)
/// and `todo_reason = "config"`, which is what lets the webapp show "broken
/// configuration" rather than a generic approval gate. `fail_item` stays for
/// genuine run failures, where the next attempt is a real chance.
fn park_config_error(shared: &Shared, item: &WorkItem, error: &str, tag: &str) {
    {
        let _q = shared.queue_mutex.lock().unwrap();
        let _ = shared.queue().mutate(&item.workid, |it| {
            it.state = workitems::STATE_TODO.into();
            it.todo_reason = workitems::TODO_REASON_CONFIG.into();
            it.lasterror = format!("CONFIGURATION ERROR — retrying cannot fix this; a human must: {}", error);
            it.attempts = it.attempts.saturating_sub(1);
            it.times.start = String::new();
        });
    }
    logging::warn(
        tag,
        &format!("{} → todo (configuration error, no attempt burned): {}", short(&item.workid), error),
    );
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
pub fn resolve_codepath(base: &Path, codepath: &str) -> PathBuf {
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

/// The mainwork turn's prompt. Normally just the request — but when the item
/// carries an ANSWERED question (features/Question_state.md), the decision is
/// prepended to it. The Q&A is composed into the prompt rather than edited into
/// `mainwork`, so the request keeps reading as itself and the pair survives as
/// the record of what was asked and what was decided.
fn mainwork_prompt(item: &WorkItem) -> String {
    if item.question.trim().is_empty() || item.answer.trim().is_empty() {
        return item.mainwork.clone();
    }
    format!(
        "# A question on this work item was answered\n\n\
         Before this run, work on this item stopped to ask a human a question.\n\
         The question and the answer are below — the answer is a decision, and it \
         outranks any assumption in the request that follows.\n\n\
         ## Asked\n\n{}\n\n## Answer\n\n{}\n\n\
         Proceed on that answer. If it is ambiguous, or acting on it raises a NEW \
         decision only a human can make, ask again with `iter ask` rather than \
         guessing.\n\n---\n\n{}",
        item.question.trim(),
        item.answer.trim(),
        item.mainwork
    )
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
        turn: Turn { label: "mainwork".into(), prompt: mainwork_prompt(item) },
        shell: None,
    });
    for entry in &item.postwork {
        let (label, prompt, shell) = resolve_prepost(&shared.project_root, entry, "postwork");
        steps.push(StepTurn { phase: Phase::Postwork, turn: Turn { label, prompt }, shell });
    }
    let shared_text =
        agents::shared_instructions(&shared.project_root, shared.cfg().globalsettings.critreview_max_rounds);
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
    let cfg = shared.cfg();
    let code_root = config::code_root(&shared.project_root, &cfg);
    let (context_files, warnings) = context::resolve(&item.context, codepath, &code_root);
    for w in &warnings {
        logging::warn(tag, w);
    }
    // Assembly order is load-bearing (features item 12): the prompt cache keys
    // on the exact BYTE PREFIX and is shared across sessions, so N agents whose
    // spin-ups begin identically cost one cache write and N-1 cheap reads. Part
    // one below is byte-identical for every item an agent type runs; part two,
    // from `# Source instructions` on, is where anything item-specific starts. A
    // single workid or resolved path above the divider gives every session its
    // own cache entry, which is what it used to do.
    let mut spinup = String::new();
    spinup.push_str(&agent.body);
    spinup.push_str(&shared_section);
    // The project head is surfaced to EVERY work item (structureV2): the
    // main.iter.md project definition first, then every globalcontextfiles
    // match — ONE spot configures all always-loaded context. Listed in full:
    // this list used to drop the entries an item also carried as its own
    // context, which made it per-item for the sake of a repeated path or two.
    let head_files = config::global_context_files(&shared.project_root);
    if !head_files.is_empty() {
        spinup.push_str(
            "\n\n# Project context ($ITER_MAINFILE + globalcontextfiles)\nThe project definition and global requirements — read what applies before starting:\n",
        );
        for f in &head_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    // ---- everything below here varies per work item ----
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
    let item_files: Vec<_> = context_files.iter().filter(|f| !head_files.contains(f)).collect();
    if !item_files.is_empty() {
        spinup.push_str("\n# Context files\nRead each of these before starting:\n");
        for f in &item_files {
            spinup.push_str(&format!("- {}\n", f.display()));
        }
    }
    if item.item_type.starts_with("test") && !item.testfiles.is_empty() {
        let (test_files, twarn) = context::resolve(&item.testfiles, codepath, &code_root);
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

    /// features/Question_state.md — the answer reaches the agent by being
    /// COMPOSED into the mainwork turn, never by editing the request. Both
    /// halves must be present: a question nobody answered yet changes nothing.
    #[test]
    fn answered_question_prefaces_the_mainwork_turn() {
        let base = WorkItem { mainwork: "Build the thing per BR-5.".into(), ..Default::default() };
        assert_eq!(mainwork_prompt(&base), base.mainwork, "no question: the request runs as written");

        let asked = WorkItem { question: "A or B?".into(), ..base.clone() };
        assert_eq!(mainwork_prompt(&asked), base.mainwork, "unanswered: nothing to preface with");

        let answered_only = WorkItem { answer: "B".into(), ..base.clone() };
        assert_eq!(mainwork_prompt(&answered_only), base.mainwork, "an answer to nothing is not a decision");

        let both = WorkItem { question: "A or B?".into(), answer: "B, and skip the cache.".into(), ..base.clone() };
        let p = mainwork_prompt(&both);
        assert!(p.contains("A or B?") && p.contains("B, and skip the cache."), "both halves reach the agent");
        assert!(p.ends_with(&base.mainwork), "the original request is still the tail of the prompt");
        assert!(p.find("A or B?") < p.find(&base.mainwork), "the decision comes BEFORE the request it governs");
        assert!(p.contains("iter ask"), "the agent is told how to escalate a follow-on decision");
    }

    /// `iter ask` parks the calling item: state, question text, refunded
    /// attempt, and a cleared stale answer from an earlier round.
    #[test]
    fn ask_parks_the_item_and_clears_the_previous_answer() {
        let root = std::env::temp_dir().join(format!("iterloop-ask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shared = test_shared(&root);
        let item = WorkItem {
            workid: "w-ask".into(),
            item_type: "code".into(),
            state: workitems::STATE_IN_PROGRESS.into(),
            question: "the FIRST question".into(),
            answer: "the first answer".into(),
            attempts: 1,
            ..Default::default()
        };
        shared.queue().append(&item).unwrap();

        // The flag round trip: written by `iter ask`, consumed exactly once.
        std::fs::write(question_path(&root, "w-ask"), "  Second question: A or B?  ").unwrap();
        let text = take_question(&root, "w-ask").expect("flag is readable");
        assert_eq!(text, "Second question: A or B?", "trimmed");
        assert!(take_question(&root, "w-ask").is_none(), "the flag is consumed, never replayed");

        ask_item(&shared, "w-ask", &text, "[mainwork] researched three options".into());
        let parked = shared.queue().load().into_iter().find(|i| i.workid == "w-ask").unwrap();
        assert_eq!(parked.state, workitems::STATE_QUESTION);
        assert_eq!(parked.question, "Second question: A or B?");
        assert_eq!(parked.answer, "", "a stale answer would read as an answer to the NEW question");
        assert_eq!(parked.attempts, 0, "asking is not a failed attempt");
        assert!(parked.output.contains("researched three options"), "the research survives as the ask's backing");
        assert!(parked.times.asked.len() > 10 && parked.times.answered.is_empty());
        assert!(
            !parked.eligible(&Config::default(), chrono::Utc::now()),
            "a parked question is never dispatched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// EVERY codepath is validated, not just the primary — a bad secondary
    /// entry used to slip through to the lock scan and livelock there.
    #[test]
    fn every_codepath_is_validated_not_just_the_first() {
        let root = std::env::temp_dir().join(format!("iterloop-cpval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("good")).unwrap();
        std::fs::create_dir_all(root.join("also")).unwrap();
        std::fs::write(root.join("good/node.code.iter.md"), "marker").unwrap();

        let mk = |paths: &[&str]| WorkItem {
            codepaths: paths.iter().map(|p| p.to_string()).collect(),
            codepath: paths[0].into(),
            ..Default::default()
        };
        assert!(validate_codepaths(&root, &mk(&["good", "also"])).is_ok(), "two real directories are fine");

        // The primary was always caught; the SECOND entry is the regression.
        let missing = validate_codepaths(&root, &mk(&["good", "gone"])).expect_err("missing secondary must fail");
        assert!(missing.contains("codepath does not exist"), "{}", missing);
        assert!(missing.contains("gone"), "the message names the offending entry: {}", missing);

        // A file is rejected with its own message — "does not exist" would send
        // the reader looking for a path that is right there.
        let file = validate_codepaths(&root, &mk(&["good", "good/node.code.iter.md"]))
            .expect_err("a file is not a lock scope");
        assert!(file.contains("is a file, not a directory"), "{}", file);
        assert!(file.contains(".iter.lock"), "the message says WHY a file cannot work: {}", file);

        // Creation-time guard is narrower: files refused, not-yet-created dirs allowed.
        assert!(reject_file_codepath(&root, &mk(&["good", "not/made/yet"])).is_ok(), "future dirs are legitimate");
        assert!(reject_file_codepath(&root, &mk(&["good/node.code.iter.md"])).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Consecutive conflicts on one item back off exponentially, so a blocked
    /// item stops returning to the picker every 15s and crowding out work that
    /// could run. Getting the lock clears the streak.
    #[test]
    fn repeated_lock_conflicts_escalate_the_backoff() {
        let root = std::env::temp_dir().join(format!("iterloop-escal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let shared = test_shared(&root);
        shared.cfg.lock().unwrap().engine.codepath_conflict_backoff_sec = 10;
        let item = WorkItem { workid: "w-block".into(), item_type: "code".into(), ..Default::default() };
        shared.queue().append(&item).unwrap();

        let wait_sec = |shared: &Shared| {
            let until = shared.deferred.lock().unwrap()[&item.workid];
            until.saturating_duration_since(Instant::now()).as_secs()
        };
        let mut seen = Vec::new();
        for _ in 0..4 {
            defer_after_conflict(&shared, &item, Path::new("/busy"), "test");
            seen.push(wait_sec(&shared));
        }
        // 10 → 20 → 40 → 80, each strictly longer than the last.
        assert!(seen.windows(2).all(|w| w[1] > w[0]), "backoff must escalate, got {:?}", seen);
        assert!(seen[0] < 11 && seen[3] >= 70, "first ~10s, fourth ~80s: {:?}", seen);
        assert_eq!(*shared.conflicts.lock().unwrap().get("w-block").unwrap(), 4);

        // The cap holds: no amount of blocking pushes the wait past the ceiling.
        for _ in 0..40 {
            defer_after_conflict(&shared, &item, Path::new("/busy"), "test");
        }
        assert!(wait_sec(&shared) <= MAX_CONFLICT_BACKOFF_SEC, "backoff is capped");

        // Winning the lock resets the streak (what run_workitem does on success).
        shared.conflicts.lock().unwrap().remove(&item.workid);
        defer_after_conflict(&shared, &item, Path::new("/busy"), "test");
        assert!(wait_sec(&shared) < 11, "a fresh conflict starts from the base backoff again");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A whole-tree scope WARNS and never refuses — it is occasionally exactly
    /// right (a cross-repo verification pass), and `**` opts out entirely.
    #[test]
    fn whole_tree_scope_warns_but_is_allowed() {
        let root = std::env::temp_dir().join(format!("iterloop-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("svc")).unwrap();
        let mk = |cp: &str, ignore: Vec<String>| WorkItem {
            codepath: cp.into(),
            codepaths: vec![cp.into()],
            codepath_ignore: ignore,
            ..Default::default()
        };
        let w = whole_tree_warning(&root, &mk(".", vec![])).expect("`.` is the whole tree");
        assert!(w.contains("LOCKS THE WHOLE TREE"), "{}", w);
        assert!(w.contains("codepath_ignore [\"**\"]"), "the warning names the way out: {}", w);
        // The absolute spelling of the same directory is the same lock.
        assert!(whole_tree_warning(&root, &mk(&root.to_string_lossy(), vec![])).is_some());
        // A scope inside the tree is ordinary work.
        assert!(whole_tree_warning(&root, &mk("svc", vec![])).is_none());
        // `**` carves out everything: the item takes no lock, so nothing to warn about.
        assert!(whole_tree_warning(&root, &mk(".", vec!["**".into()])).is_none());
        // A narrower carve-out is still a whole-tree lock.
        assert!(whole_tree_warning(&root, &mk(".", vec!["svc/tests/".into()])).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The picker records WHY it skipped each candidate, so a queue full of
    /// runnable-looking work with one agent on it has a visible cause.
    #[test]
    fn picker_publishes_why_it_skipped_each_candidate() {
        let root = std::env::temp_dir().join(format!("iterloop-blk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["held", "held/inner", "free"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let shared = test_shared(&root);
        let mk = |id: &str, cp: &str, prio: i64| WorkItem {
            workid: id.into(),
            item_type: "code".into(),
            priority: prio,
            codepath: root.join(cp).to_string_lossy().into_owned(),
            ..Default::default()
        };
        // An on-disk lock over `held/` (as another engine would leave).
        let _lock = locks::acquire_codepath_lock(&root.join("held"), "w-holder", "code", 600, &[]).unwrap();
        shared.queue().append(&mk("w-under", "held/inner", 1)).unwrap();
        shared.queue().append(&mk("w-free", "free", 9)).unwrap();

        // The blocked item sorts FIRST on priority, so a silent skip is exactly
        // the case that looks like the engine ignoring urgent work.
        let picked = pick_next(&shared, &["code"], false).expect("the free item still runs");
        assert_eq!(picked.workid, "w-free", "a locked scope must not stall the queue behind it");

        let blocked = shared.blocked.lock().unwrap().clone();
        let b = blocked.get("w-under").expect("the skipped item's reason is recorded");
        assert_eq!(b.by, "w-holder", "the chip names WHO holds the lock");
        assert_eq!(b.kind, "lock", "an on-disk lock, not an item running in this engine");
        assert!(b.path.ends_with("held"), "and the scope that covers it: {}", b.path);
        assert!(!blocked.contains_key("w-free"), "a runnable item is not reported as blocked");
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
            conflicts: Mutex::new(HashMap::new()),
            blocked: Mutex::new(HashMap::new()),
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
            .insert("running".into(), vec![(root.join("a").canonicalize().unwrap(), Vec::new())]);
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
            .insert("running-code".into(), vec![(root.join("obj").canonicalize().unwrap(), vec!["test/".into()])]);
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
            .insert("running-tw".into(), vec![(root.join("obj/test").canonicalize().unwrap(), Vec::new())]);
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

    /// Issue 1: a codepath that does not exist is a configuration error, and no
    /// number of retries makes a directory appear. It parks for a human with the
    /// attempt refunded — the pdy-dev item that burned 50 attempts over five
    /// days on this exact message is the reason.
    #[test]
    fn missing_codepath_parks_for_a_human_instead_of_burning_attempts() {
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
        assert_eq!(items[0].state, workitems::STATE_TODO, "parked for review, not failed and retried");
        assert_eq!(items[0].todo_reason, "config", "the UI must tell this apart from an approval gate");
        assert_eq!(items[0].attempts, 0, "the run never started; the attempt is refunded");
        assert!(items[0].lasterror.contains("codepath does not exist"));
        assert!(items[0].lasterror.contains("does/not/exist"), "the message names the path to fix");
        assert!(items[0].lasterror.contains("CONFIGURATION ERROR"), "and says a retry cannot help");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The file-as-codepath case is the same validation family and gets the same
    /// treatment: a file can never hold the `.iter.lock` a codepath needs.
    #[test]
    fn a_file_codepath_parks_too() {
        let root = std::env::temp_dir().join(format!("iterloop-filecp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shared = Arc::new(test_shared(&root));
        std::fs::write(root.join("notadir.md"), "content").unwrap();
        let item = WorkItem {
            workid: "w-filecp".into(),
            item_type: "code".into(),
            codepath: "notadir.md".into(),
            attempts: 3,
            ..Default::default()
        };
        shared.queue().append(&item).unwrap();
        run_workitem(Arc::clone(&shared), AgentDef::default(), item, "test#2".into());
        let items = shared.queue().load();
        assert_eq!(items[0].state, workitems::STATE_TODO);
        assert_eq!(items[0].todo_reason, "config");
        assert_eq!(items[0].attempts, 2);
        assert!(items[0].lasterror.contains("is a file, not a directory"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Picking an item clears whatever parked it: a running item must not still
    /// wear a "broken configuration" chip from the round before.
    #[test]
    fn claiming_an_item_clears_its_todo_reason() {
        let root = std::env::temp_dir().join(format!("iterloop-clearreason-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let shared = test_shared(&root);
        let item = WorkItem {
            workid: "w-refixed".into(),
            item_type: "code".into(),
            state: workitems::STATE_QUEUED.into(),
            todo_reason: "config".into(),
            ..Default::default()
        };
        shared.queue().append(&item).unwrap();
        let picked = pick_next(&shared, &["code"], false).expect("a re-queued item is pickable");
        assert!(picked.todo_reason.is_empty());
        assert!(shared.queue().load()[0].todo_reason.is_empty(), "and on disk, not just in the claim");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue 12c: the item decides the model when it says so, the agent type
    /// decides otherwise. Whitespace is not an opinion.
    #[test]
    fn item_model_overrides_the_agent_default() {
        let agent = AgentDef { type_name: "code".into(), model: "opus".into(), ..Default::default() };
        assert_eq!(effective_model(&agent, &WorkItem::default()), "opus");
        let cheap = WorkItem { model: "sonnet".into(), ..Default::default() };
        assert_eq!(effective_model(&agent, &cheap), "sonnet");
        let blank = WorkItem { model: "   ".into(), ..Default::default() };
        assert_eq!(effective_model(&agent, &blank), "opus", "an empty override is no override");
    }

    /// Issue 12d: the prompt cache keys on the exact byte prefix and is shared
    /// across sessions, so the spin-up two items of the same type get must be
    /// byte-identical until the item-specific part starts. Interpolate a workid
    /// or a resolved path above `# Work item` and this goes red.
    #[test]
    fn spinup_is_byte_identical_until_the_work_item_section() {
        let root = std::env::temp_dir().join(format!("iterloop-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".iter/agents")).unwrap();
        std::fs::write(root.join(".iter/agents/_shared.md"), "# Shared rules\nWrite tests first.").unwrap();
        std::fs::write(root.join("main.iter.md"), "# The project").unwrap();
        let shared = test_shared(&root);
        let agent = AgentDef {
            type_name: "code".into(),
            body: "You are the code agent. Do the work.".into(),
            ..Default::default()
        };
        let mk = |id: &str, title: &str| WorkItem {
            workid: id.into(),
            item_type: "code".into(),
            title: title.into(),
            mainwork: format!("do the {} thing", title),
            lasterror: format!("{} died last time", id),
            output: "half of it".into(),
            ..Default::default()
        };
        let first = build_turns(&shared, &agent, &mk("aaaa1111-w", "Alpha"), &root, "t#1");
        let second = build_turns(&shared, &agent, &mk("bbbb2222-w", "Beta"), &root, "t#2");
        let (pa, pb) = (&first[0].turn.prompt, &second[0].turn.prompt);
        let common: String =
            pa.chars().zip(pb.chars()).take_while(|(x, y)| x == y).map(|(x, _)| x).collect();

        assert!(common.contains(&agent.body), "the persona is in the shared prefix");
        assert!(common.contains("Write tests first."), "and so is _shared.md");
        assert!(common.contains("main.iter.md"), "and the always-loaded project context");
        assert!(!common.contains("aaaa1111-w"), "nothing item-specific may reach the shared prefix");

        // The divider is the work-item header: everything above it is shared.
        let divider = pa.find("\n\n# Work item\n").expect("the work item section exists");
        assert!(divider <= common.len(), "the prefix must run all the way to the item header");
        // ...and the item's own material really is below it.
        assert!(pa[divider..].contains("aaaa1111-w"));
        assert!(pa[divider..].contains("# Previous attempt"), "retry context is per-item, so it goes below");
        assert!(pa.ends_with("do the Alpha thing"), "the request is still the last thing the agent reads");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Issue 6, the engine half: one hold stands for every worker that hits the
    /// same closed window, only the first announces it, and a later reset
    /// extends the hold rather than shortening it.
    #[test]
    fn concurrent_limit_errors_fold_into_one_hold() {
        let root = std::env::temp_dir().join(format!("iterloop-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let shared = test_shared(&root);
        let now = chrono::Utc::now();

        let (announced, until) =
            shared.enter_limit_hold(limits::Hold::from_error("You've hit your session limit", now, 300), now);
        assert!(announced, "the first worker through says it");
        assert_eq!(until, now + chrono::Duration::seconds(300));

        // A second agent, one second behind, is told the window is already shut.
        let (announced, until) = shared
            .enter_limit_hold(limits::Hold::from_error("You've hit your session limit", now, 60), now);
        assert!(!announced, "no second announcement — this is the log spam the issue is about");
        assert_eq!(until, now + chrono::Duration::seconds(300), "a shorter guess must not shorten the hold");

        // A third names a real reset further out: that wins, and makes the hold
        // authoritative, but still does not re-announce.
        let far = now + chrono::Duration::hours(2);
        let (announced, until) = shared.enter_limit_hold(
            limits::Hold { until: far, authoritative: true, set_at: now },
            now,
        );
        assert!(!announced);
        assert_eq!(until, far);
        let held = shared.limit_hold.lock().unwrap().clone().expect("still holding");
        assert!(held.authoritative, "the stated reset now governs the hold");

        // Once the window has actually reopened, the next error opens a NEW
        // hold, which is announced again.
        let later = far + chrono::Duration::seconds(1);
        let (announced, _) =
            shared.enter_limit_hold(limits::Hold::from_error("session limit", later, 300), later);
        assert!(announced, "a fresh window closure is news again");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn topdir_setting_rebases_relative_codepaths() {
        // structureV2: {topdir} from .iter/config.iter.json replaced the old
        // code_root setting — the pdy iter-in-a-subdir layout.
        let root = std::env::temp_dir().join(format!("iterloop-croot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let engine_home = root.join("devops");
        std::fs::create_dir_all(engine_home.join(".iter/.engine")).unwrap();
        std::fs::create_dir_all(root.join("core/repos/comp")).unwrap();

        // Default: topdir = the engine home (parent of .iter/).
        let cfg = Config::default();
        let base = config::code_root(&engine_home, &cfg);
        assert_eq!(base, engine_home.canonicalize().unwrap());

        // topdir "{thisfiledir}/../../" rebases to the parent.
        std::fs::write(
            crate::project::config_path(&engine_home),
            r#"{ "topdir": "{thisfiledir}/../../" }"#,
        )
        .unwrap();
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
