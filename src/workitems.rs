use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::locks;

pub const STATE_TODO: &str = "todo";
pub const STATE_QUEUED: &str = "queued";
pub const STATE_IN_PROGRESS: &str = "in-progress";
pub const STATE_PAUSED: &str = "paused";
pub const STATE_FAILED: &str = "failed";
pub const STATE_COMPLETE: &str = "complete";
/// A schedule template (itersched.rs): never picked by the loop — itersched
/// clones it into queued runs on cadence. Pausing it stops the schedule;
/// completing it retires the schedule.
pub const STATE_SCHEDULED: &str = "scheduled";

pub const EXEC_AGENT: &str = "agent";
pub const EXEC_SHELL: &str = "shell";

/// Automation mode (features/workitem_automation.md): how this lineage's
/// OFFSPRING are born. "review" — children land `todo` (human gate per
/// stage); "auto" — children land `queued` (fully automated build). Unset
/// inherits the creating parent's mode at add time; a user-created item with
/// no mode means "review".
pub const AUTOMATION_REVIEW: &str = "review";
pub const AUTOMATION_AUTO: &str = "auto";

/// When a `scheduled` template fires (itersched.rs): the schedule spec.
/// Minute granularity by design — the check cadence is 59s.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Sched {
    /// "every" (every_min minutes) | "daily" (at HH:MM tz) | "weekly" (day + at
    /// HH:MM tz) | "stale" (when no clone has COMPLETED within every_min minutes).
    pub kind: String,
    pub every_min: u64,
    /// "HH:MM" 24h, for daily/weekly.
    pub at: String,
    /// "mon".."sun", for weekly.
    pub day: String,
    /// IANA timezone; empty = globalsettings.user_timezone.
    pub tz: String,
    /// ISO timestamp of the last fire (clone creation) — the durable restart
    /// memory; the audit trail is .iter/.engine/sched_log.jsonl.
    pub last_fired: String,
}

impl Sched {
    pub fn is_none(&self) -> bool {
        self.kind.is_empty()
    }
}

fn is_agent_exec(s: &str) -> bool {
    s.is_empty() || s == EXEC_AGENT
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Times {
    pub added: String,
    pub start: String,
    pub preworkdone: String,
    pub mainworkdone: String,
    pub postworkdone: String,
    pub closed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkItem {
    pub workid: String,
    pub title: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub state: String,
    pub source: String,
    pub priority: i64,
    pub risk: i64,
    pub codepath: String,
    /// Gitignore-like patterns (relative to codepath) carved OUT of this item's lock
    /// scope: the item neither locks nor may edit those subtrees, so another item can
    /// work there in parallel (e.g. code owns `pth/object/` ignoring `test/` while a
    /// testwriter owns `pth/object/test/`).
    pub codepath_ignore: Vec<String>,
    /// Provenance of sweep-born fix items: the testgroup label this item exists to
    /// turn green. Dedup key (one open item per group), the `--broken`/`--fixed`
    /// target, and the UI's run-history → workitem link. Empty on ordinary items.
    pub source_testgroup: String,
    /// Informational snapshot: which tests were red when this item was born.
    /// Diagnosis starting point only — enforcement is group-level.
    pub source_tests: Vec<String>,
    /// Provenance of itersched-born runs: the workid of the `scheduled` template
    /// this item was cloned from. Dedup key — one OPEN clone per schedule.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source_schedule: String,
    /// Executor: "agent" (default — a claude session) or "shell" (the engine runs
    /// prework/mainwork/postwork lines as shell commands directly; no LLM).
    #[serde(skip_serializing_if = "is_agent_exec")]
    pub exec: String,
    /// The schedule spec; only meaningful while state == "scheduled".
    #[serde(skip_serializing_if = "Sched::is_none")]
    pub sched: Sched,
    /// Ordering gate: full workids this item must wait for. A queued item is
    /// not dispatchable until every dependency is SATISFIED — closed complete,
    /// and (unless depends_on_shallow) every item the dependency created is
    /// itself closed complete, transitively. Evaluated before lock checks;
    /// gates dispatch of QUEUED items only (a todo item stays parked as usual).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Opt-out of transitive satisfaction: wait for the named items' own
    /// completion only, ignoring the items they created (a plan item
    /// "completes" the moment it spawns children — rarely what a caller wants).
    #[serde(skip_serializing_if = "is_false")]
    pub depends_on_shallow: bool,
    /// Engine-recorded provenance: the workid of the work item whose agent ran
    /// the `iter add` that created this item (from $ITER_WORKID). This is what
    /// makes transitive dependency satisfaction possible.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub created_by: String,
    /// "review" | "auto" (or "" = unset → inherit parent / default review):
    /// whether items THIS item creates are born `todo` (human gate) or
    /// `queued` (fully automated). Engine-enforced on agent-sourced adds —
    /// prompts do not decide state; guards (reject, non-convergence, failed
    /// deps) still outrank it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub automation: String,
    /// Engine-recorded at each run start: HEAD of the item's codepath repo the
    /// moment work began (empty if the codepath is not in a git repo). The
    /// undo point offered when a run is stopped mid-stream —
    /// `git reset --hard <this>` discards everything the run did.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub git_start_commit: String,
    pub context: Vec<String>,
    pub testfiles: Vec<String>,
    pub prework: Vec<String>,
    pub mainwork: String,
    pub postwork: Vec<String>,
    pub output: String,
    pub attempts: u32,
    pub lasterror: String,
    pub times: Times,
}

impl Default for WorkItem {
    fn default() -> Self {
        WorkItem {
            workid: String::new(),
            title: String::new(),
            item_type: String::new(),
            state: STATE_QUEUED.into(),
            source: "user".into(),
            priority: 5,
            risk: 0,
            codepath: ".".into(),
            codepath_ignore: Vec::new(),
            source_testgroup: String::new(),
            source_tests: Vec::new(),
            source_schedule: String::new(),
            exec: EXEC_AGENT.into(),
            sched: Sched::default(),
            depends_on: Vec::new(),
            depends_on_shallow: false,
            created_by: String::new(),
            automation: String::new(),
            git_start_commit: String::new(),
            context: Vec::new(),
            testfiles: Vec::new(),
            prework: Vec::new(),
            mainwork: String::new(),
            postwork: Vec::new(),
            output: String::new(),
            attempts: 0,
            lasterror: String::new(),
            times: Times::default(),
        }
    }
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

impl WorkItem {
    /// Effective priority for ordering. Priorities are LOWER-IS-SOONER (P0 =
    /// most urgent, P10 = least, default 5 — inverted 2026-08-17 to match the
    /// industry P0..Pn convention). Error-sourced work gets a -2 boost, and
    /// failed items awaiting retry get -2 so they go before queued work of the
    /// same priority instead of languishing behind a deep backlog.
    pub fn effective_priority(&self) -> i64 {
        self.priority
            - if self.source == "error" { 2 } else { 0 }
            - if self.state == STATE_FAILED { 2 } else { 0 }
    }

    /// Is this item eligible to be picked up right now?
    pub fn eligible(&self, cfg: &Config, now: DateTime<Utc>) -> bool {
        match self.state.as_str() {
            STATE_QUEUED => true,
            STATE_FAILED => {
                if self.attempts >= cfg.engine.max_attempts {
                    return false;
                }
                match parse_iso(&self.times.start) {
                    Some(start) => {
                        now >= start + chrono::Duration::seconds(cfg.engine.retry_backoff_sec as i64)
                    }
                    None => true,
                }
            }
            _ => false,
        }
    }

    /// Terminal failure: attempts exhausted.
    pub fn failed_terminally(&self, cfg: &Config) -> bool {
        self.state == STATE_FAILED && self.attempts >= cfg.engine.max_attempts
    }
}

/// The last-12-characters view of a workid — what the webapp header shows and
/// the suffix convention dependency notes use.
pub fn short12(workid: &str) -> String {
    let chars: Vec<char> = workid.chars().collect();
    chars[chars.len().saturating_sub(12)..].iter().collect()
}

/// Where one item's `depends_on` stands right now, against the open queue and
/// the closed archive.
#[derive(Debug, Clone, PartialEq)]
pub enum DepStatus {
    /// Every dependency closed complete (descendants included unless shallow).
    Satisfied,
    /// Waiting: the named workid (a dependency or one of its descendants) is
    /// still open. The item stays visibly queued.
    Blocked(String),
    /// This dependency can never satisfy (closed failed, or missing) — the
    /// message says which and why. The engine flips the dependent to `todo`.
    Failed(String),
}

/// Evaluate an item's dependency gate. First non-satisfied dependency wins.
pub fn dep_status(item: &WorkItem, open: &[WorkItem], closed: &[WorkItem]) -> DepStatus {
    for dep in &item.depends_on {
        match one_dep_status(dep, item.depends_on_shallow, open, closed) {
            DepStatus::Satisfied => {}
            other => return other,
        }
    }
    DepStatus::Satisfied
}

/// SATISFIED means the dependency closed COMPLETE — and its descendants too:
/// a plan item "completes" the moment it spawns children, so unless shallow,
/// every item the dependency created (via the engine-recorded `created_by`)
/// must itself be closed complete, transitively. A dependency (or descendant)
/// that closed FAILED never releases the dependent — that surfaces as Failed
/// so the engine can flip the dependent to `todo` for human review, never a
/// silent run on a broken foundation and never a silent hang.
fn one_dep_status(dep: &str, shallow: bool, open: &[WorkItem], closed: &[WorkItem]) -> DepStatus {
    if open.iter().any(|i| i.workid == dep) {
        return DepStatus::Blocked(dep.to_string());
    }
    let Some(done) = closed.iter().rev().find(|i| i.workid == dep) else {
        return DepStatus::Failed(format!(
            "dependency {} found in neither the open queue nor the closed archive",
            short12(dep)
        ));
    };
    if done.state != STATE_COMPLETE {
        return DepStatus::Failed(format!("dependency {} closed {}", short12(dep), done.state));
    }
    if shallow {
        return DepStatus::Satisfied;
    }
    let mut stack = vec![dep.to_string()];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(child) = open.iter().find(|i| i.created_by == id) {
            return DepStatus::Blocked(child.workid.clone());
        }
        for child in closed.iter().filter(|i| i.created_by == id) {
            if child.state != STATE_COMPLETE {
                return DepStatus::Failed(format!(
                    "descendant {} of dependency {} closed {}",
                    short12(&child.workid),
                    short12(dep),
                    child.state
                ));
            }
            stack.push(child.workid.clone());
        }
    }
    DepStatus::Satisfied
}

/// Resolve each `depends_on` entry — a full workid or any unambiguous suffix
/// (the convention is the last 12 characters, what the webapp header shows) —
/// against open and closed items, then refuse cycles. An unknown or ambiguous
/// suffix REFUSES the add rather than guessing; a dependency may name a closed
/// item (satisfied immediately — useful for idempotent re-adds). On success
/// `item.depends_on` holds deduplicated full workids.
pub fn resolve_depends_on(item: &mut WorkItem, open: &[WorkItem], closed: &[WorkItem]) -> Result<(), String> {
    let mut resolved: Vec<String> = Vec::new();
    for entry in &item.depends_on {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut matches: Vec<&str> = open
            .iter()
            .chain(closed.iter())
            .map(|i| i.workid.as_str())
            .filter(|id| *id == entry || id.ends_with(entry))
            .collect();
        matches.sort();
        matches.dedup();
        match matches.len() {
            0 => return Err(format!("depends_on \"{}\" matches no work item (open or closed)", entry)),
            1 => {
                let id = matches[0].to_string();
                if id == item.workid {
                    return Err(format!("depends_on \"{}\" is the item itself — a cycle of one", entry));
                }
                if !resolved.contains(&id) {
                    resolved.push(id);
                }
            }
            n => {
                return Err(format!(
                    "depends_on \"{}\" is ambiguous ({} work items end with it — use a longer suffix)",
                    entry, n
                ))
            }
        }
    }
    item.depends_on = resolved;
    if let Some(path) = find_cycle(item, open) {
        return Err(format!("depends_on creates a cycle: {}", path.join(" → ")));
    }
    Ok(())
}

/// Walk the dependency graph from `item` through the OPEN items' `depends_on`
/// edges (closed items gate nothing, so no cycle can run through them). Any id
/// revisited on the current path is a cycle; the returned path names it.
fn find_cycle(item: &WorkItem, open: &[WorkItem]) -> Option<Vec<String>> {
    let mut edges: std::collections::HashMap<&str, &[String]> =
        open.iter().map(|i| (i.workid.as_str(), i.depends_on.as_slice())).collect();
    edges.insert(item.workid.as_str(), item.depends_on.as_slice());

    fn walk<'a>(
        id: &'a str,
        edges: &std::collections::HashMap<&'a str, &'a [String]>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        if let Some(pos) = path.iter().position(|p| *p == id) {
            let mut cycle: Vec<String> = path[pos..].iter().map(|s| short12(s)).collect();
            cycle.push(short12(id));
            return Some(cycle);
        }
        path.push(id);
        if let Some(deps) = edges.get(id) {
            for dep in deps.iter() {
                if let Some(cycle) = walk(dep.as_str(), edges, path) {
                    return Some(cycle);
                }
            }
        }
        path.pop();
        None
    }
    walk(item.workid.as_str(), &edges, &mut Vec::new())
}

/// The work-item queue: `.iter/.engine/workitems.jsonl` plus the append-only
/// `workitems_closed.jsonl`. All writes go through the record-lock protocol; within
/// this process an additional mutex (held by the caller, see scheduler) serializes use.
pub struct Queue {
    pub open_path: PathBuf,
    pub closed_path: PathBuf,
    cfg: Config,
}

impl Queue {
    pub fn new(project_root: &Path, cfg: &Config) -> Queue {
        let dir = crate::config::engine_dir(project_root);
        Queue {
            open_path: dir.join("workitems.jsonl"),
            closed_path: dir.join("workitems_closed.jsonl"),
            cfg: cfg.clone(),
        }
    }

    pub fn load(&self) -> Vec<WorkItem> {
        let text = std::fs::read_to_string(&self.open_path).unwrap_or_default();
        let mut items = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<WorkItem>(line) {
                Ok(item) => items.push(item),
                Err(e) => eprintln!(
                    "warning: {}:{} is not a valid workitem ({}); line skipped",
                    self.open_path.display(),
                    i + 1,
                    e
                ),
            }
        }
        items
    }

    /// (byte length, mtime) fingerprint used for cheap change detection.
    #[allow(dead_code)]
    pub fn fingerprint(&self) -> (u64, Option<std::time::SystemTime>) {
        match std::fs::metadata(&self.open_path) {
            Ok(m) => (m.len(), m.modified().ok()),
            Err(_) => (0, None),
        }
    }

    /// The ONE mutation path: hold the record lock across load → modify → write, so
    /// concurrent writers (engine workers, the API server, `iter add` from agents)
    /// can never save over each other's changes.
    pub fn with_lock<T>(&self, f: impl FnOnce(&mut Vec<WorkItem>) -> T) -> std::io::Result<T> {
        let _guard = locks::acquire_file_lock(
            &self.open_path,
            self.cfg.engine.queue_lock_retry_ms,
            self.cfg.engine.queue_lock_break_sec,
        )?;
        let mut items = self.load();
        let result = f(&mut items);
        let mut text = String::new();
        for item in &items {
            text.push_str(&serde_json::to_string(item).expect("workitem serializes"));
            text.push('\n');
        }
        let tmp = self.open_path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, &self.open_path)?;
        Ok(result)
    }

    #[allow(dead_code)]
    pub fn save(&self, items: &[WorkItem]) -> std::io::Result<()> {
        let replacement: Vec<WorkItem> = items.to_vec();
        self.with_lock(move |list| *list = replacement)
    }

    /// Append one item under the record-lock protocol (external-producer path).
    pub fn append(&self, item: &WorkItem) -> std::io::Result<()> {
        let item = item.clone();
        self.with_lock(move |items| items.push(item))
    }

    pub fn append_closed(&self, item: &WorkItem) -> std::io::Result<()> {
        let mut line = serde_json::to_string(item).expect("workitem serializes");
        line.push('\n');
        append_to(&self.closed_path, &line)
    }

    pub fn load_closed(&self) -> Vec<WorkItem> {
        let text = std::fs::read_to_string(&self.closed_path).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<WorkItem>(l).ok())
            .collect()
    }

    /// Apply `f` to the item with `workid` under the lock. Returns false if not found.
    pub fn mutate(&self, workid: &str, f: impl FnOnce(&mut WorkItem)) -> std::io::Result<bool> {
        self.with_lock(|items| match items.iter_mut().find(|i| i.workid == workid) {
            Some(item) => {
                f(item);
                true
            }
            None => false,
        })
    }

    /// Close an item out: remove from the open queue, append to the closed file.
    pub fn close(&self, item: &WorkItem) -> std::io::Result<()> {
        self.with_lock(|items| items.retain(|i| i.workid != item.workid))?;
        self.append_closed(item)
    }
}

fn append_to(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

/// Startup crash recovery: any in-progress item was orphaned by a dead engine.
pub fn recover_orphans(queue: &Queue) -> std::io::Result<usize> {
    queue.with_lock(|items| {
        let mut count = 0;
        for item in items.iter_mut() {
            if item.state == STATE_IN_PROGRESS {
                item.state = STATE_QUEUED.into();
                item.lasterror = format!("orphaned in-progress at engine startup {}", now_iso());
                count += 1;
            }
        }
        count
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iterloop-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter/.engine")).unwrap();
        dir
    }

    /// The shipped reference project's seed queue (sampleV1/), covering the
    /// three ways an item gets into a real queue: added by a user, born from a
    /// test sweep, and a schedule template waiting to be cloned.
    #[test]
    fn parses_sample_seed_queue() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sampleV1");
        let cfg = Config::default();
        let queue = Queue::new(&root, &cfg);
        let items = queue.load();
        assert_eq!(items.len(), 6, "3 user items, 2 sweep-born, 1 schedule template");

        let user: Vec<_> = items.iter().filter(|i| i.source == "user").collect();
        assert_eq!(user.len(), 3);
        assert!(user.iter().all(|i| i.state == STATE_QUEUED));
        assert!(user.iter().all(|i| !i.times.added.is_empty()));

        // The sweep's authoring items land in todo — new-test authoring is gated.
        let swept: Vec<_> = items.iter().filter(|i| i.source == "testsweep").collect();
        assert_eq!(swept.len(), 2);
        assert!(swept.iter().all(|i| i.item_type == "testwriter" && i.state == STATE_TODO));
        assert!(swept.iter().all(|i| !i.source_testgroup.is_empty()), "dedup key rides along");

        // The Test Loop: a shell-executed schedule template, never picked by the
        // loop itself — itersched clones it on cadence.
        let sched = items.iter().find(|i| i.state == STATE_SCHEDULED).expect("a schedule template");
        assert_eq!(sched.title, "Test Loop");
        assert_eq!(sched.exec, EXEC_SHELL);
        assert_eq!(sched.sched.kind, "every");
        assert_eq!(sched.sched.every_min, 120);
        assert!(sched.mainwork.contains("testsweep"));
        assert_eq!(sched.codepath_ignore, vec!["**"], "run-only: carves out everything, so it locks nothing");

        // The dependency gate is seeded too.
        let dependent = items.iter().find(|i| !i.depends_on.is_empty()).expect("a gated item");
        assert!(dependent.depends_on_shallow);

        assert_eq!(queue.load_closed().len(), 1, "one closed item seeds the archive");
    }

    #[test]
    fn roundtrip_save_load_mutate_close() {
        let root = tmpdir("roundtrip");
        let cfg = Config::default();
        let queue = Queue::new(&root, &cfg);
        let mut item = WorkItem { workid: "w1".into(), title: "t".into(), item_type: "code".into(), ..Default::default() };
        item.times.added = now_iso();
        queue.append(&item).unwrap();
        queue.append(&WorkItem { workid: "w2".into(), item_type: "test".into(), ..Default::default() }).unwrap();

        assert_eq!(queue.load().len(), 2);
        assert!(queue.mutate("w1", |i| i.state = STATE_IN_PROGRESS.into()).unwrap());
        assert_eq!(queue.load()[0].state, STATE_IN_PROGRESS);

        let mut done = queue.load()[0].clone();
        done.state = STATE_COMPLETE.into();
        queue.close(&done).unwrap();
        assert_eq!(queue.load().len(), 1);
        let closed = std::fs::read_to_string(&queue.closed_path).unwrap();
        assert!(closed.contains("\"w1\""));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn eligibility_rules() {
        let cfg = Config::default();
        let now = Utc::now();
        let mut item = WorkItem::default();
        assert!(item.eligible(&cfg, now));
        item.state = STATE_PAUSED.into();
        assert!(!item.eligible(&cfg, now));
        item.state = STATE_TODO.into();
        assert!(!item.eligible(&cfg, now));

        item.state = STATE_FAILED.into();
        item.attempts = 1;
        item.times.start = now_iso();
        assert!(!item.eligible(&cfg, now), "failed item within backoff is not eligible");
        item.times.start = (now - chrono::Duration::seconds(301)).to_rfc3339_opts(SecondsFormat::Secs, true);
        assert!(item.eligible(&cfg, now), "failed item past backoff is eligible");
        item.attempts = cfg.engine.max_attempts;
        assert!(!item.eligible(&cfg, now), "attempts exhausted is terminal");
        assert!(item.failed_terminally(&cfg));
    }

    // Priorities are lower-is-sooner: a SMALLER effective_priority wins the pick.
    #[test]
    fn error_source_priority_boost() {
        let mut a = WorkItem { priority: 5, ..Default::default() };
        a.source = "error".into();
        let b = WorkItem { priority: 4, ..Default::default() };
        assert!(a.effective_priority() < b.effective_priority(), "error source outranks a P4");
    }

    #[test]
    fn failed_state_priority_boost() {
        let failed = WorkItem { priority: 5, state: STATE_FAILED.into(), ..Default::default() };
        let queued_same = WorkItem { priority: 5, state: STATE_QUEUED.into(), ..Default::default() };
        let queued_more_urgent = WorkItem { priority: 2, state: STATE_QUEUED.into(), ..Default::default() };
        assert!(failed.effective_priority() < queued_same.effective_priority(), "retry goes before equal queued work");
        assert!(queued_more_urgent.effective_priority() < failed.effective_priority(), "a genuinely urgent item still wins");
    }

    // The dependency gate (workitem_dependency.md): every guard proves it can fail.
    #[test]
    fn dependency_gate_states() {
        let b = WorkItem { workid: "b".into(), depends_on: vec!["a".into()], ..Default::default() };
        // Dependency still open (any state) → blocked, visibly waiting.
        let open = vec![WorkItem { workid: "a".into(), state: STATE_IN_PROGRESS.into(), ..Default::default() }, b.clone()];
        assert_eq!(dep_status(&b, &open, &[]), DepStatus::Blocked("a".into()));
        // Closed complete with no descendants → satisfied.
        let a_done = WorkItem { workid: "a".into(), state: STATE_COMPLETE.into(), ..Default::default() };
        assert_eq!(dep_status(&b, &[b.clone()], &[a_done]), DepStatus::Satisfied);
        // Closed failed NEVER releases the dependent.
        let a_failed = WorkItem { workid: "a".into(), state: STATE_FAILED.into(), ..Default::default() };
        assert!(matches!(dep_status(&b, &[b.clone()], &[a_failed]), DepStatus::Failed(_)));
        // A missing dependency is a visible failure, not a silent hang.
        assert!(matches!(dep_status(&b, &[b.clone()], &[]), DepStatus::Failed(_)));
    }

    #[test]
    fn transitive_descendants_gate() {
        // A plan item "completes" the moment it spawns children: A closed
        // complete but its created child C is still open → B stays blocked.
        let mut b = WorkItem { workid: "b".into(), depends_on: vec!["a".into()], ..Default::default() };
        let a_done = WorkItem { workid: "a".into(), state: STATE_COMPLETE.into(), ..Default::default() };
        let c_open = WorkItem { workid: "c".into(), created_by: "a".into(), ..Default::default() };
        assert_eq!(dep_status(&b, &[b.clone(), c_open.clone()], &[a_done.clone()]), DepStatus::Blocked("c".into()));
        // C closes complete → satisfied; an open grandchild re-blocks.
        let c_done = WorkItem { workid: "c".into(), state: STATE_COMPLETE.into(), created_by: "a".into(), ..Default::default() };
        assert_eq!(dep_status(&b, &[b.clone()], &[a_done.clone(), c_done.clone()]), DepStatus::Satisfied);
        let g_open = WorkItem { workid: "g".into(), created_by: "c".into(), ..Default::default() };
        assert_eq!(
            dep_status(&b, &[b.clone(), g_open], &[a_done.clone(), c_done.clone()]),
            DepStatus::Blocked("g".into())
        );
        // A descendant that closed failed poisons the gate — no silent run on
        // a broken foundation.
        let g_failed = WorkItem { workid: "g".into(), state: STATE_FAILED.into(), created_by: "c".into(), ..Default::default() };
        assert!(matches!(
            dep_status(&b, &[b.clone()], &[a_done.clone(), c_done, g_failed]),
            DepStatus::Failed(_)
        ));
        // The shallow flag inverts: A's own completion is enough.
        b.depends_on_shallow = true;
        assert_eq!(dep_status(&b, &[b.clone(), c_open], &[a_done]), DepStatus::Satisfied);
    }

    #[test]
    fn depends_on_resolution_and_cycles() {
        let mk = |id: &str| WorkItem { workid: id.into(), ..Default::default() };
        let open = vec![
            mk("11111111-aaaa-bbbb-cccc-123456789abc"),
            mk("22222222-aaaa-bbbb-cccc-aaaaaaaa9abc"),
        ];
        let closed =
            vec![WorkItem { workid: "33333333-dddd-eeee-ffff-fedcba987654".into(), state: STATE_COMPLETE.into(), ..Default::default() }];
        // The last-12 convention (what the webapp header shows) resolves.
        let mut item = WorkItem { workid: "new".into(), depends_on: vec!["123456789abc".into()], ..Default::default() };
        resolve_depends_on(&mut item, &open, &closed).unwrap();
        assert_eq!(item.depends_on, vec!["11111111-aaaa-bbbb-cccc-123456789abc".to_string()]);
        // A dependency may name a closed item (satisfied immediately).
        let mut item = WorkItem { workid: "new".into(), depends_on: vec!["fedcba987654".into()], ..Default::default() };
        resolve_depends_on(&mut item, &open, &closed).unwrap();
        assert_eq!(item.depends_on, vec!["33333333-dddd-eeee-ffff-fedcba987654".to_string()]);
        // Ambiguous suffix REFUSED, never guessed; unknown refused too.
        let mut item = WorkItem { workid: "new".into(), depends_on: vec!["9abc".into()], ..Default::default() };
        assert!(resolve_depends_on(&mut item, &open, &closed).unwrap_err().contains("ambiguous"));
        let mut item = WorkItem { workid: "new".into(), depends_on: vec!["deadbeef".into()], ..Default::default() };
        assert!(resolve_depends_on(&mut item, &open, &closed).unwrap_err().contains("matches no work item"));
        // Self-dependency is a cycle of one.
        let mut item = mk("11111111-aaaa-bbbb-cccc-123456789abc");
        item.depends_on = vec!["123456789abc".into()];
        assert!(resolve_depends_on(&mut item, &open, &closed).unwrap_err().contains("itself"));
        // A→B→A refused with the cycle path named.
        let mut b = mk("bbbbbbbb-2222-3333-4444-bbbbbbbbbbbb");
        b.depends_on = vec!["aaaaaaaa-2222-3333-4444-aaaaaaaaaaaa".into()];
        let mut a = mk("aaaaaaaa-2222-3333-4444-aaaaaaaaaaaa");
        a.depends_on = vec!["bbbbbbbbbbbb".into()];
        let err = resolve_depends_on(&mut a, &[b], &[]).unwrap_err();
        assert!(err.contains("cycle") && err.contains("aaaaaaaaaaaa") && err.contains("bbbbbbbbbbbb"), "{}", err);
    }

    #[test]
    fn recover_orphaned_in_progress() {
        let root = tmpdir("recover");
        let cfg = Config::default();
        let queue = Queue::new(&root, &cfg);
        queue.append(&WorkItem { workid: "w1".into(), state: STATE_IN_PROGRESS.into(), ..Default::default() }).unwrap();
        let n = recover_orphans(&queue).unwrap();
        assert_eq!(n, 1);
        assert_eq!(queue.load()[0].state, STATE_QUEUED);
        let _ = std::fs::remove_dir_all(&root);
    }
}
