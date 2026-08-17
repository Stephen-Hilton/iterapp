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
    /// Effective priority for ordering: error-sourced work gets a +2 boost,
    /// and failed items awaiting retry get +2 so they go before queued work
    /// of the same priority instead of languishing behind a deep backlog.
    pub fn effective_priority(&self) -> i64 {
        self.priority
            + if self.source == "error" { 2 } else { 0 }
            + if self.state == STATE_FAILED { 2 } else { 0 }
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

    #[test]
    fn parses_sample_seed_queue() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let cfg = Config::default();
        let queue = Queue::new(&root, &cfg);
        let items = queue.load();
        assert_eq!(items.len(), 3, "expected 3 seed items (no test items — the sweep runs tests)");
        let code = items.iter().find(|i| i.item_type == "code").unwrap();
        assert_eq!(code.state, STATE_QUEUED);
        assert_eq!(code.prework, vec!["git-pull"]);
        assert!(!code.times.added.is_empty());
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

    #[test]
    fn error_source_priority_boost() {
        let mut a = WorkItem { priority: 5, ..Default::default() };
        a.source = "error".into();
        let b = WorkItem { priority: 6, ..Default::default() };
        assert!(a.effective_priority() > b.effective_priority());
    }

    #[test]
    fn failed_state_priority_boost() {
        let failed = WorkItem { priority: 5, state: STATE_FAILED.into(), ..Default::default() };
        let queued_same = WorkItem { priority: 5, state: STATE_QUEUED.into(), ..Default::default() };
        let queued_higher = WorkItem { priority: 8, state: STATE_QUEUED.into(), ..Default::default() };
        assert!(failed.effective_priority() > queued_same.effective_priority());
        assert!(queued_higher.effective_priority() > failed.effective_priority());
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
