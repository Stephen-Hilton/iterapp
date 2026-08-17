use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::testgroups::{self, TestGroup};
use crate::workitems;

/// The whole engine⇄script contract, from the exit code alone (features/TDD.md
/// "Test Contract"): 0 = ran, all as expected; 1 = ran, something unexpected;
/// anything else (crash, missing dep, timeout kill) = the script itself broke —
/// a distinct state so infrastructure problems never masquerade as red tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Green,
    Red,
    Error,
}

impl Outcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Green => "passed",
            Outcome::Red => "failed",
            Outcome::Error => "error",
        }
    }
}

#[derive(Debug)]
pub struct TestRunResult {
    pub id: String,
    pub name: String,
    pub shell: String,
    pub outcome: Outcome,
    pub exit_code: i32,
    pub pass: u64,
    pub total: u64,
    pub log_path: PathBuf,
    /// Human-readable note for `error` outcomes (timeout, spawn failure, …).
    pub detail: String,
}

#[derive(Debug)]
pub struct GroupRunResult {
    pub label: String,
    pub tg_file: PathBuf,
    pub test_dir: PathBuf,
    pub outcome: Outcome,
    pub pass: u64,
    pub total: u64,
    pub runs: Vec<TestRunResult>,
    /// True when every registered test ran (no --test filter): only then do the
    /// group's lastrun/result/counts get updated — a filtered run proves nothing
    /// about the group.
    pub full_run: bool,
}

/// Find the testgroup `label` across every testgroup.iter.md under `code_root`.
/// Duplicate labels are reported (first wins) so they get rationalized.
pub fn locate_group(code_root: &Path, label: &str) -> Result<(PathBuf, TestGroup), String> {
    let mut found: Vec<(PathBuf, TestGroup)> = Vec::new();
    for file in testgroups::find_files(code_root) {
        let Ok(content) = std::fs::read_to_string(&file) else { continue };
        for g in testgroups::parse(&content) {
            if g.label == label {
                found.push((file.clone(), g));
            }
        }
    }
    match found.len() {
        0 => Err(format!("testgroup \"{}\" not found under {}", label, code_root.display())),
        1 => Ok(found.into_iter().next().unwrap()),
        n => {
            eprintln!(
                "warning: testgroup \"{}\" defined in {} files; using {}",
                label,
                n,
                found[0].0.display()
            );
            Ok(found.into_iter().next().unwrap())
        }
    }
}

/// Run a testgroup's shell scripts (optionally narrowed to one test id/script by
/// `filter`), capture each run to `<test_dir>/runs/<timestamp>-<id>.log`, and on a
/// full run update the group's JSONL block (lastrun/result/counts) in place.
/// The group shares one wall-clock budget (`testing.test_sweep_timeout_minutes`);
/// scripts that would start past the deadline are recorded as `error` without running.
pub fn run_group(
    cfg: &Config,
    tg_file: &Path,
    label: &str,
    filter: Option<&str>,
) -> Result<GroupRunResult, String> {
    let content =
        std::fs::read_to_string(tg_file).map_err(|e| format!("cannot read {}: {}", tg_file.display(), e))?;
    let mut groups = testgroups::parse(&content);
    let group = groups
        .iter()
        .find(|g| g.label == label)
        .ok_or_else(|| format!("testgroup \"{}\" not in {}", label, tg_file.display()))?
        .clone();
    let test_dir = tg_file.parent().ok_or("testgroup file has no parent directory")?.to_path_buf();

    let entries: Vec<_> = group
        .testlist
        .iter()
        .filter(|t| filter.map(|f| t.id == f || t.shell == f).unwrap_or(true))
        .cloned()
        .collect();
    if entries.is_empty() {
        return Err(match filter {
            Some(f) => format!("no test matching \"{}\" in testgroup \"{}\"", f, label),
            None => format!("testgroup \"{}\" has no tests registered", label),
        });
    }
    let full_run = filter.is_none();

    let runs_dir = test_dir.join("runs");
    std::fs::create_dir_all(&runs_dir).map_err(|e| format!("cannot create {}: {}", runs_dir.display(), e))?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let budget = Duration::from_secs(cfg.testing.test_sweep_timeout_minutes.max(1) * 60);
    let started = Instant::now();

    let mut runs = Vec::new();
    for entry in entries {
        let log_path = runs_dir.join(format!("{}-{}.log", stamp, sanitize(&entry.id)));
        let remaining = budget.saturating_sub(started.elapsed());
        let script = test_dir.join(&entry.shell);
        let run = if remaining.is_zero() {
            let detail = format!("not run: group budget ({} min) exhausted", cfg.testing.test_sweep_timeout_minutes);
            write_log(&log_path, &entry.shell, -1, "", "", &detail);
            TestRunResult {
                id: entry.id,
                name: entry.name,
                shell: entry.shell,
                outcome: Outcome::Error,
                exit_code: -1,
                pass: 0,
                total: 1,
                log_path,
                detail,
            }
        } else if !script.is_file() {
            let detail = format!("script not found: {}", script.display());
            write_log(&log_path, &entry.shell, -1, "", "", &detail);
            TestRunResult {
                id: entry.id,
                name: entry.name,
                shell: entry.shell,
                outcome: Outcome::Error,
                exit_code: -1,
                pass: 0,
                total: 1,
                log_path,
                detail,
            }
        } else {
            let (exit_code, stdout, stderr, timed_out) = run_script(&script, &test_dir, remaining);
            let detail = if timed_out {
                format!("timed out (group budget {} min); killed", cfg.testing.test_sweep_timeout_minutes)
            } else {
                String::new()
            };
            write_log(&log_path, &entry.shell, exit_code, &stdout, &stderr, &detail);
            let outcome = match exit_code {
                0 => Outcome::Green,
                1 => Outcome::Red,
                _ => Outcome::Error,
            };
            // Per-test counts from the contract's ITER_RESULT trailer; a script
            // that doesn't report counts is counted as one test.
            let (pass, total) = parse_iter_result(&stdout).unwrap_or_else(|| match outcome {
                Outcome::Green => (1, 1),
                _ => (0, 1),
            });
            TestRunResult {
                id: entry.id,
                name: entry.name,
                shell: entry.shell,
                outcome,
                exit_code,
                pass,
                total,
                log_path,
                detail,
            }
        };
        runs.push(run);
    }

    let outcome = if runs.iter().any(|r| r.outcome == Outcome::Error) {
        Outcome::Error
    } else if runs.iter().any(|r| r.outcome == Outcome::Red) {
        Outcome::Red
    } else {
        Outcome::Green
    };
    let pass: u64 = runs.iter().map(|r| r.pass).sum();
    let total: u64 = runs.iter().map(|r| r.total).sum();

    if full_run {
        for g in groups.iter_mut() {
            if g.label == label {
                g.lastrun = workitems::now_iso();
                g.result = outcome.as_str().into();
                g.counts = format!("{}/{}", pass, total);
            }
        }
        let updated = testgroups::update(&content, &groups);
        std::fs::write(tg_file, updated).map_err(|e| format!("cannot update {}: {}", tg_file.display(), e))?;
    }

    Ok(GroupRunResult { label: label.into(), tg_file: tg_file.to_path_buf(), test_dir, outcome, pass, total, runs, full_run })
}

/// `bash <script>` in `cwd` with a hard deadline. The script leads its own process
/// group so a timeout kill takes its whole tree. Returns (exit code, stdout,
/// stderr, timed_out); spawn failures surface as exit -1 with the error on stderr.
fn run_script(script: &Path, cwd: &Path, timeout: Duration) -> (i32, String, String, bool) {
    let mut cmd = Command::new("bash");
    cmd.arg(script).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (-1, String::new(), format!("cannot spawn bash {}: {}", script.display(), e), false),
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(output)) => (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            false,
        ),
        Ok(Err(e)) => (-1, String::new(), format!("wait failed: {}", e), false),
        Err(_) => {
            if cfg!(unix) {
                let _ = Command::new("sh").arg("-c").arg(format!("kill -9 -{}", pid)).status();
            } else {
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
            (-1, String::new(), String::new(), true)
        }
    }
}

/// The machine-readable trailer: last stdout line of the form
/// `ITER_RESULT pass=12 fail=2 total=14` (fail is tolerated but derived data —
/// pass/total are what the engine records).
fn parse_iter_result(stdout: &str) -> Option<(u64, u64)> {
    let line = stdout.lines().rev().find(|l| l.trim_start().starts_with("ITER_RESULT"))?;
    let mut pass = None;
    let mut total = None;
    for token in line.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            match k {
                "pass" => pass = v.parse().ok(),
                "total" => total = v.parse().ok(),
                _ => {}
            }
        }
    }
    Some((pass?, total?))
}

/// Verbatim capture for the runs/ history: everything the diagnosing agent (or the
/// UI's run browser) needs from one script execution.
fn write_log(path: &Path, shell: &str, exit_code: i32, stdout: &str, stderr: &str, detail: &str) {
    let mut body = format!(
        "# iterapp test run\n# script: {}\n# time: {}\n# exit: {}\n",
        shell,
        workitems::now_iso(),
        exit_code
    );
    if !detail.is_empty() {
        body.push_str(&format!("# note: {}\n", detail));
    }
    body.push_str("\n--- stdout ---\n");
    body.push_str(stdout);
    body.push_str("\n--- stderr ---\n");
    body.push_str(stderr);
    if let Err(e) = std::fs::write(path, body) {
        eprintln!("warning: cannot write run log {}: {}", path.display(), e);
    }
}

fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testgroups::TestEntry;

    fn setup(name: &str, scripts: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("iter-runtests-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let test_dir = root.join("comp/test");
        std::fs::create_dir_all(&test_dir).unwrap();
        for (file, body) in scripts {
            std::fs::write(test_dir.join(file), body).unwrap();
        }
        let entries: Vec<TestEntry> = scripts
            .iter()
            .map(|(file, _)| TestEntry {
                id: file.trim_end_matches(".sh").into(),
                name: (*file).into(),
                desc: String::new(),
                shell: (*file).into(),
            })
            .collect();
        let group = TestGroup { label: "g1".into(), testlist: entries, ..Default::default() };
        let content = testgroups::update("# tests\n", &[group]);
        let tg_file = test_dir.join("testgroup.iter.md");
        std::fs::write(&tg_file, content).unwrap();
        (root, tg_file)
    }

    #[test]
    fn green_red_error_exit_codes() {
        let (root, tg) = setup(
            "codes",
            &[
                ("t1.sh", "echo 'ITER_RESULT pass=3 fail=0 total=3'\nexit 0\n"),
                ("t2.sh", "echo 'ITER_RESULT pass=1 fail=1 total=2'\nexit 1\n"),
                ("t3.sh", "exit 7\n"),
            ],
        );
        let cfg = Config::default();
        let run = run_group(&cfg, &tg, "g1", None).unwrap();
        assert_eq!(run.outcome, Outcome::Error, "error trumps red");
        assert_eq!(run.runs[0].outcome, Outcome::Green);
        assert_eq!(run.runs[1].outcome, Outcome::Red);
        assert_eq!(run.runs[2].outcome, Outcome::Error);
        assert_eq!((run.pass, run.total), (4, 6), "ITER_RESULT counts aggregate; t3 counts as 0/1");
        // Block updated + logs written.
        let content = std::fs::read_to_string(&tg).unwrap();
        let groups = testgroups::parse(&content);
        assert_eq!(groups[0].result, "error");
        assert_eq!(groups[0].counts, "4/6");
        assert!(!groups[0].lastrun.is_empty());
        assert!(run.runs[0].log_path.is_file());
        let log = std::fs::read_to_string(&run.runs[0].log_path).unwrap();
        assert!(log.contains("ITER_RESULT"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn all_green_marks_group_passed() {
        let (root, tg) = setup("green", &[("t1.sh", "exit 0\n"), ("t2.sh", "echo ok\nexit 0\n")]);
        let cfg = Config::default();
        let run = run_group(&cfg, &tg, "g1", None).unwrap();
        assert_eq!(run.outcome, Outcome::Green);
        assert_eq!((run.pass, run.total), (2, 2), "no ITER_RESULT → 1 test per script");
        let groups = testgroups::parse(&std::fs::read_to_string(&tg).unwrap());
        assert!(groups[0].is_green());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn filtered_run_does_not_update_group() {
        let (root, tg) = setup("filter", &[("t1.sh", "exit 0\n"), ("t2.sh", "exit 1\n")]);
        let cfg = Config::default();
        let run = run_group(&cfg, &tg, "g1", Some("t1")).unwrap();
        assert!(!run.full_run);
        assert_eq!(run.outcome, Outcome::Green);
        assert_eq!(run.runs.len(), 1);
        let groups = testgroups::parse(&std::fs::read_to_string(&tg).unwrap());
        assert!(groups[0].result.is_empty(), "filtered run must not stamp the group");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_script_is_error_not_red() {
        let (root, tg) = setup("missing", &[("t1.sh", "exit 0\n")]);
        // Register a second test whose script does not exist.
        let content = std::fs::read_to_string(&tg).unwrap();
        let mut groups = testgroups::parse(&content);
        groups[0].testlist.push(TestEntry { id: "ghost".into(), name: "ghost".into(), desc: String::new(), shell: "ghost.sh".into() });
        std::fs::write(&tg, testgroups::update(&content, &groups)).unwrap();
        let cfg = Config::default();
        let run = run_group(&cfg, &tg, "g1", None).unwrap();
        assert_eq!(run.outcome, Outcome::Error);
        assert!(run.runs[1].detail.contains("script not found"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn locate_group_finds_by_label() {
        let (root, tg) = setup("locate", &[("t1.sh", "exit 0\n")]);
        let (found, group) = locate_group(&root, "g1").unwrap();
        assert_eq!(found, tg);
        assert_eq!(group.label, "g1");
        assert!(locate_group(&root, "nope").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
