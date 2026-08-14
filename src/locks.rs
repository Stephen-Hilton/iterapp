use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CODEPATH_LOCK_NAME: &str = ".iter.lock";

// ---------------------------------------------------------------------------
// Record lock: guards writes to a file (the queue) via a `<file>.lock` sentinel
// created with create-exclusive semantics.
// ---------------------------------------------------------------------------

pub struct FileLockGuard {
    lock_path: PathBuf,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Acquire `<target>.lock`. Retries every `retry_ms`. Ghost-breaker: if the lock has
/// been held for `break_sec` AND the target file's (len, mtime) fingerprint has not
/// changed over that whole window, the holder is presumed dead and the lock is broken.
pub fn acquire_file_lock(target: &Path, retry_ms: u64, break_sec: u64) -> std::io::Result<FileLockGuard> {
    let lock_path = PathBuf::from(format!("{}.lock", target.display()));
    let mut waited_ms: u64 = 0;
    let mut baseline: Option<(u64, Option<std::time::SystemTime>)> = None;
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path) {
            Ok(_) => return Ok(FileLockGuard { lock_path }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let fp = fingerprint(target);
                match baseline {
                    None => baseline = Some(fp),
                    Some(base) => {
                        if waited_ms >= break_sec * 1000 {
                            if base == fp {
                                // Held past the break window with zero progress on the
                                // target: ghost process. Force the lock free.
                                let _ = std::fs::remove_file(&lock_path);
                            }
                            baseline = Some(fp);
                            waited_ms = 0;
                            continue;
                        }
                        if base != fp {
                            // Target moved: the holder is alive; restart the window.
                            baseline = Some(fp);
                            waited_ms = 0;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(retry_ms));
                waited_ms += retry_ms;
            }
            Err(e) => return Err(e),
        }
    }
}

fn fingerprint(path: &Path) -> (u64, Option<std::time::SystemTime>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    }
}

// ---------------------------------------------------------------------------
// Codepath lock: `.iter.lock` file guarding a directory tree an agent is editing.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CodepathLockInfo {
    pub workid: String,
    pub agent: String,
    pub pid: u32,
    pub created: String,
    pub timeout: String,
}

impl CodepathLockInfo {
    pub fn active(&self, now: DateTime<Utc>) -> bool {
        match crate::workitems::parse_iso(&self.timeout) {
            Some(t) => now < t,
            // Unparseable timeout: treat as stale so a corrupt lock can't wedge the tree.
            None => false,
        }
    }
}

pub struct CodepathLock {
    pub path: PathBuf,
    pub workid: String,
}

impl CodepathLock {
    #[allow(dead_code)]
    pub fn release(self) {
        // Drop does the work.
    }
}

impl Drop for CodepathLock {
    fn drop(&mut self) {
        // Only delete a lock we own (workid match) — see codepath_unlock.md.
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if let Ok(info) = serde_json::from_str::<CodepathLockInfo>(&text) {
                if info.workid == self.workid {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }
}

/// Scan for an active `.iter.lock` covering `codepath`: the directory itself, all
/// descendants, and all ancestors up to the filesystem root. Expired locks are deleted
/// on sight. Returns the path of the first active lock found.
pub fn find_active_lock(codepath: &Path, now: DateTime<Utc>) -> Option<PathBuf> {
    if let Some(p) = find_ancestor_lock(codepath, now) {
        return Some(p);
    }
    // Descendants.
    scan_descendants(codepath, now)
}

/// Ancestor-chain-only lock probe (codepath itself and every parent): cheap enough
/// to run per queue candidate per tick, unlike the full descendant walk. Used by the
/// scheduler to skip un-runnable items without wasting an agent slot on them.
pub fn find_ancestor_lock(codepath: &Path, now: DateTime<Utc>) -> Option<PathBuf> {
    let mut dir = Some(codepath.to_path_buf());
    while let Some(d) = dir {
        if let Some(p) = check_lock_file(&d.join(CODEPATH_LOCK_NAME), now) {
            return Some(p);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

fn scan_descendants(dir: &Path, now: DateTime<Utc>) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            if let Some(p) = scan_descendants(&path, now) {
                return Some(p);
            }
        } else if name == CODEPATH_LOCK_NAME {
            if let Some(p) = check_lock_file(&path, now) {
                return Some(p);
            }
        }
    }
    None
}

/// Returns Some(path) if the lock file exists and is active; deletes it if expired.
fn check_lock_file(path: &Path, now: DateTime<Utc>) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<CodepathLockInfo>(&text) {
        Ok(info) if info.active(now) => Some(path.to_path_buf()),
        _ => {
            // Expired or corrupt: clear it per codepath_lock.md step 2.
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

/// Acquire the codepath lock for a work item, or return the conflicting lock path.
pub fn acquire_codepath_lock(
    codepath: &Path,
    workid: &str,
    agent: &str,
    timeout_sec: u64,
) -> Result<CodepathLock, PathBuf> {
    let now = Utc::now();
    if let Some(conflict) = find_active_lock(codepath, now) {
        return Err(conflict);
    }
    let info = CodepathLockInfo {
        workid: workid.to_string(),
        agent: agent.to_string(),
        pid: std::process::id(),
        created: crate::workitems::now_iso(),
        timeout: (now + Duration::seconds(timeout_sec as i64))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let path = codepath.join(CODEPATH_LOCK_NAME);
    let text = serde_json::to_string_pretty(&info).expect("lock info serializes");
    if std::fs::write(&path, text).is_err() {
        return Err(path);
    }
    Ok(CodepathLock { path, workid: workid.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iterloop-lock-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn record_lock_contention_two_threads() {
        let dir = tmpdir("record");
        let target = dir.join("queue.jsonl");
        std::fs::write(&target, "").unwrap();
        let t2 = target.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..50 {
                let _g = acquire_file_lock(&t2, 5, 60).unwrap();
                let n: u64 = std::fs::read_to_string(&t2).unwrap().trim().parse().unwrap_or(0);
                std::fs::write(&t2, format!("{}", n + 1)).unwrap();
            }
        });
        for _ in 0..50 {
            let _g = acquire_file_lock(&target, 5, 60).unwrap();
            let n: u64 = std::fs::read_to_string(&target).unwrap().trim().parse().unwrap_or(0);
            std::fs::write(&target, format!("{}", n + 1)).unwrap();
        }
        handle.join().unwrap();
        let n: u64 = std::fs::read_to_string(&target).unwrap().trim().parse().unwrap();
        assert_eq!(n, 100, "every increment must survive the interleaving");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ghost_break_frees_dead_lock() {
        let dir = tmpdir("ghost");
        let target = dir.join("queue.jsonl");
        std::fs::write(&target, "stable").unwrap();
        // A ghost lock nobody will ever release:
        std::fs::write(format!("{}.lock", target.display()), "").unwrap();
        // break_sec=1 so the test is fast.
        let start = std::time::Instant::now();
        let _g = acquire_file_lock(&target, 50, 1).unwrap();
        assert!(start.elapsed() >= std::time::Duration::from_millis(900), "must wait out the break window");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codepath_parent_child_conflicts() {
        let dir = tmpdir("codepath");
        let child = dir.join("sub");
        std::fs::create_dir_all(&child).unwrap();

        let lock = acquire_codepath_lock(&dir, "w1", "code", 600).unwrap();
        // Child blocked by ancestor lock:
        assert!(acquire_codepath_lock(&child, "w2", "code", 600).is_err());
        drop(lock);

        // Now lock the child; parent must be blocked by descendant lock:
        let lock = acquire_codepath_lock(&child, "w2", "code", 600).unwrap();
        assert!(acquire_codepath_lock(&dir, "w3", "code", 600).is_err());
        drop(lock);

        // Both released: parent lockable again.
        assert!(acquire_codepath_lock(&dir, "w4", "code", 600).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_codepath_lock_is_cleared() {
        let dir = tmpdir("expired");
        let info = CodepathLockInfo {
            workid: "dead".into(),
            agent: "code".into(),
            pid: 0,
            created: "2020-01-01T00:00:00Z".into(),
            timeout: "2020-01-01T01:00:00Z".into(),
        };
        let lock_file = dir.join(CODEPATH_LOCK_NAME);
        std::fs::write(&lock_file, serde_json::to_string(&info).unwrap()).unwrap();
        assert!(find_active_lock(&dir, Utc::now()).is_none());
        assert!(!lock_file.exists(), "expired lock must be deleted on sight");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drop_only_deletes_own_lock() {
        let dir = tmpdir("own");
        let lock = acquire_codepath_lock(&dir, "w1", "code", 600).unwrap();
        // Simulate another owner overwriting the lock file:
        let other = CodepathLockInfo { workid: "other".into(), agent: "test".into(), pid: 1, created: crate::workitems::now_iso(), timeout: crate::workitems::now_iso() };
        std::fs::write(dir.join(CODEPATH_LOCK_NAME), serde_json::to_string(&other).unwrap()).unwrap();
        drop(lock);
        assert!(dir.join(CODEPATH_LOCK_NAME).exists(), "someone else's lock must survive our drop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
