use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config::{self, Config};
use crate::locks;
use crate::logging;
use crate::markers;
use crate::runtests::{self, GroupRunResult, Outcome};
use crate::testgroups::{self, TestGroup};
use crate::workitems::{self, Queue, WorkItem};

/// Source tag on sweep-born work items. Deliberately NOT "error": error-sourced
/// items get a hard-coded +2 effective-priority boost, while sweep items must obey
/// the configured testing priorities (default below user work, filling idle time).
pub const SOURCE: &str = "testsweep";

/// One sweepable unit: a testgroup DECLARED by a C4 object's marker file. The
/// marker file defines the C4 object; test ownership flows from its `testgroup:`
/// key, never from where a testgroup.iter.md happens to sit. No key = that C4
/// object is deliberately outside the sweep.
#[derive(Debug)]
struct Candidate {
    tg_file: PathBuf,
    group: TestGroup,
    priority: i64,
    /// The C4 object's directory (= the marker file's directory): the fix item's
    /// codepath / lock scope.
    object_dir: PathBuf,
    /// Subtree carved out of the fix item's lock scope (the testwriter's turf),
    /// relative to object_dir, e.g. "test/" or "tests/".
    carve: String,
    /// For the fix-item prompt: which marker declared this group.
    marker_path: String,
}

#[derive(Debug, Default)]
pub struct SweepReport {
    pub examined: usize,
    pub ran: usize,
    pub green: usize,
    pub red: usize,
    pub error: usize,
    pub items_created: usize,
    pub items_refreshed: usize,
    pub stale_closed: usize,
    /// C4 objects whose marker file has no `testgroup:` key — deliberately untested.
    pub undeclared: usize,
    pub notes: Vec<String>,
}

impl SweepReport {
    pub fn summary(&self) -> String {
        format!(
            "test sweep: {} declared group(s) examined, {} ran ({} green, {} red, {} error); {} item(s) created, {} refreshed, {} stale auto-closed; {} C4 object(s) without a testgroup: key",
            self.examined, self.ran, self.green, self.red, self.error, self.items_created, self.items_refreshed, self.stale_closed, self.undeclared
        )
    }
}

/// One deterministic sweep (features/TDD.md "Engine Test Sweep"). Discovery is
/// marker-driven: scan for marker files (same roots/glob as the Projects view),
/// follow each `level:` marker's MANDATORY `testgroup:` key to its
/// testgroup.iter.md, re-run groups not provably green-and-fresh (respecting
/// `parallel_test_concurrency`), record every run, and translate results into work
/// items — red → per-group fix item scoped to the C4 object's directory (queued/todo
/// per the group's auto_fix), error → testwriter repair item in todo — with a dedup
/// guard on `source_testgroup` and auto-close of unstarted sweep items whose group
/// came back green. testgroup.iter.md files no marker claims are reported, never run.
pub fn sweep(project_root: &Path, cfg: &Config) -> SweepReport {
    let mut report = SweepReport::default();
    let code_root = config::code_root(project_root, cfg);
    let now = chrono::Utc::now();
    let stale_cutoff = now - chrono::Duration::hours(cfg.testing.test_green_stale_hours.max(1) as i64);

    // Same marker universe the Projects view sees (projects.json scan_roots + glob),
    // through the ONE shared resolver (~ expansion included).
    let settings = crate::server::project_settings(project_root);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md").to_string();
    let roots = crate::server::scan_roots(project_root);
    let scan = markers::scan(project_root, &roots, &glob);

    let mut claimed: Vec<PathBuf> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for node in &scan.nodes {
        if node.testgroup.is_empty() {
            report.undeclared += 1;
            continue;
        }
        let object_dir = PathBuf::from(&node.dir);
        let tg_file = object_dir.join(&node.testgroup);
        let tg_file = match tg_file.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                report.notes.push(format!(
                    "\"{}\": marker declares testgroup: {} but the file does not exist — a testwriter item should create it",
                    node.name, node.testgroup
                ));
                continue;
            }
        };
        claimed.push(tg_file.clone());
        // The carve: declared test_dir, else the declared testgroups file's own
        // subdirectory (when it has one), else the global default.
        let carve = if !node.test_dir.is_empty() {
            node.test_dir.trim_end_matches('/').to_string()
        } else {
            tg_file
                .parent()
                .and_then(|p| p.strip_prefix(&object_dir).ok())
                .map(|r| r.to_string_lossy().into_owned())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| cfg.globalsettings.test_dir.clone())
        };
        let Ok(content) = std::fs::read_to_string(&tg_file) else { continue };
        for group in testgroups::parse(&content) {
            report.examined += 1;
            if group.testlist.is_empty() {
                // Tests not written yet is a plan/testwriter concern, not a red run.
                report.notes.push(format!("skip \"{}\": no tests registered", group.label));
                continue;
            }
            let fresh_green = group.is_green()
                && workitems::parse_iso(&group.lastrun).map(|t| t > stale_cutoff).unwrap_or(false);
            if fresh_green {
                continue;
            }
            let priority = if group.is_green() {
                cfg.testing.workitem_priority_lastrun_green // was green, went stale
            } else {
                cfg.testing.workitem_priority_lastrun_not_green // never/no-longer green
            };
            // An agent working anywhere in the C4 object makes results meaningless
            // (and a testwriter may be mid-edit on the scripts): skip until quiet.
            if let Some(lock) = locks::find_active_lock(&object_dir, &[], now) {
                report.notes.push(format!("skip \"{}\": codepath busy ({})", group.label, lock.display()));
                continue;
            }
            candidates.push(Candidate {
                tg_file: tg_file.clone(),
                group,
                priority,
                object_dir: object_dir.clone(),
                carve: carve.clone(),
                marker_path: node.path.clone(),
            });
        }
    }

    // testgroup.iter.md files no marker file claims: surfaced, never run.
    for file in testgroups::find_files(&code_root) {
        let canon = file.canonicalize().unwrap_or(file.clone());
        if !claimed.contains(&canon) {
            report.notes.push(format!(
                "unowned testgroup.iter.md (no marker file declares it via testgroup:): {}",
                file.display()
            ));
        }
    }

    // Run candidates through the deterministic runner, a few groups at a time.
    let work: Arc<Mutex<Vec<Candidate>>> = Arc::new(Mutex::new(candidates));
    let results: Arc<Mutex<Vec<(Candidate, Result<GroupRunResult, String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let workers = cfg.testing.parallel_test_concurrency.max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let work = Arc::clone(&work);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let next = work.lock().unwrap().pop();
                let Some(cand) = next else { break };
                let run = runtests::run_group(cfg, &cand.tg_file, &cand.group.label, None);
                results.lock().unwrap().push((cand, run));
            });
        }
    });
    let results = Arc::try_unwrap(results).expect("workers joined").into_inner().unwrap();

    // Translate results into queue actions.
    let queue = Queue::new(project_root, cfg);
    for (cand, run) in results {
        let run = match run {
            Ok(r) => r,
            Err(e) => {
                report.notes.push(format!("\"{}\": {}", cand.group.label, e));
                continue;
            }
        };
        report.ran += 1;
        match run.outcome {
            Outcome::Green => {
                report.green += 1;
                report.stale_closed += close_stale_items(&queue, &cand.group.label);
            }
            Outcome::Red | Outcome::Error => {
                if run.outcome == Outcome::Red {
                    report.red += 1;
                } else {
                    report.error += 1;
                }
                match ensure_fix_item(project_root, cfg, &queue, &cand, &run) {
                    Ok(Action::Created) => report.items_created += 1,
                    Ok(Action::Refreshed) => report.items_refreshed += 1,
                    Ok(Action::Deduped) => {}
                    Err(e) => report.notes.push(format!("\"{}\": {}", cand.group.label, e)),
                }
            }
        }
    }
    report
}

enum Action {
    Created,
    Refreshed,
    Deduped,
}

/// Green group: any sweep-born item still sitting unstarted (todo/queued, zero
/// attempts) is stale — the deterministic, zero-agent stale-close. Items an agent
/// has started are left alone.
fn close_stale_items(queue: &Queue, label: &str) -> usize {
    let closed: Vec<WorkItem> = queue
        .with_lock(|items| {
            let mut out = Vec::new();
            items.retain(|i| {
                let stale = i.source_testgroup == label
                    && i.source == SOURCE
                    && (i.state == workitems::STATE_TODO || i.state == workitems::STATE_QUEUED)
                    && i.attempts == 0;
                if stale {
                    let mut done = i.clone();
                    done.state = workitems::STATE_COMPLETE.into();
                    done.output = format!(
                        "auto-closed by test sweep: testgroup \"{}\" is green again; item was stale (never started)",
                        label
                    );
                    done.times.closed = workitems::now_iso();
                    out.push(done);
                }
                !stale
            });
            out
        })
        .unwrap_or_default();
    for item in &closed {
        if let Err(e) = queue.append_closed(item) {
            logging::error("sweep", &format!("cannot archive auto-closed item {}: {}", item.workid, e));
        } else {
            logging::info("sweep", &format!("auto-closed stale item {} (\"{}\" green again)", item.workid, label));
        }
    }
    closed.len()
}

/// Red/error group: exactly one open item per testgroup. An existing open item
/// with the same `source_testgroup` (whoever created it) suppresses creation;
/// while it is still unstarted its `source_tests` snapshot is refreshed in place.
fn ensure_fix_item(
    project_root: &Path,
    cfg: &Config,
    queue: &Queue,
    cand: &Candidate,
    run: &GroupRunResult,
) -> Result<Action, String> {
    let failing: Vec<String> =
        run.runs.iter().filter(|r| r.outcome != Outcome::Green).map(|r| r.id.clone()).collect();
    let open_count = queue.load().len();
    let item = build_fix_item(project_root, cfg, cand, run, &failing);

    let action = queue
        .with_lock(|items| {
            if let Some(existing) = items.iter_mut().find(|i| i.source_testgroup == cand.group.label) {
                if existing.attempts == 0
                    && (existing.state == workitems::STATE_TODO || existing.state == workitems::STATE_QUEUED)
                    && existing.source == SOURCE
                    && existing.source_tests != failing
                {
                    existing.source_tests = failing.clone();
                    return Action::Refreshed;
                }
                return Action::Deduped;
            }
            if open_count >= cfg.engine.max_open_workitems {
                return Action::Deduped; // full queue: suppressed, next sweep retries
            }
            items.push(item.clone());
            Action::Created
        })
        .map_err(|e| e.to_string())?;
    if matches!(action, Action::Created) {
        logging::info(
            "sweep",
            &format!(
                "created {} item {} for testgroup \"{}\" ({}, state {})",
                item.item_type, item.workid, cand.group.label, run.outcome.as_str(), item.state
            ),
        );
    }
    Ok(action)
}

fn build_fix_item(
    project_root: &Path,
    cfg: &Config,
    cand: &Candidate,
    run: &GroupRunResult,
    failing: &[String],
) -> WorkItem {
    let code_root = config::code_root(project_root, cfg);
    let group = &cand.group;
    let rel = |p: &Path| {
        p.strip_prefix(&code_root)
            .map(|r| {
                let s = r.to_string_lossy().into_owned();
                if s.is_empty() { ".".to_string() } else { s }
            })
            .unwrap_or_else(|_| p.to_string_lossy().into_owned())
    };

    let failing_lines: String = run
        .runs
        .iter()
        .filter(|r| r.outcome != Outcome::Green)
        .map(|r| {
            format!(
                "- {} ({}): {} — {} — log: {}\n",
                r.id,
                r.name,
                r.outcome.as_str(),
                if r.detail.is_empty() { format!("exit {}", r.exit_code) } else { r.detail.clone() },
                r.log_path.display()
            )
        })
        .collect();

    let mut item = WorkItem {
        workid: uuid::Uuid::new_v4().to_string(),
        source: SOURCE.into(),
        priority: cand.priority,
        source_testgroup: group.label.clone(),
        source_tests: failing.to_vec(),
        context: vec![run.tg_file.to_string_lossy().into_owned(), cand.marker_path.clone()],
        ..Default::default()
    };
    item.times.added = workitems::now_iso();

    if run.outcome == Outcome::Error {
        // The scripts themselves broke: a testwriter repairs the tests, and a human
        // reviews first — infrastructure failures never auto-dispatch.
        item.item_type = "testwriter".into();
        item.state = workitems::STATE_TODO.into();
        item.title = format!("repair broken test script(s) in testgroup \"{}\"", group.label);
        item.codepath = rel(&cand.object_dir.join(&cand.carve));
        item.mainwork = format!(
            "The deterministic test sweep could not RUN parts of testgroup \"{label}\" — script errors \
             (exit codes other than 0/1), not failing tests.\n\nBroken:\n{failing_lines}\n\
             Repair the test scripts so they honor the contract: exit 0 = all as expected, exit 1 = \
             some assertion unexpected, anything else = script failure; last stdout line \
             `ITER_RESULT pass=X fail=Y total=Z`. The tests are registered in {tg_file} \
             (declared by the marker file {marker}). Verify with: \
             \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\"",
            label = group.label,
            failing_lines = failing_lines,
            tg_file = run.tg_file.display(),
            marker = cand.marker_path,
        );
    } else {
        item.item_type = "code".into();
        item.state = if group.auto_fix { workitems::STATE_QUEUED.into() } else { workitems::STATE_TODO.into() };
        item.title = format!("fix red testgroup \"{}\" ({} failing)", group.label, failing.len());
        item.codepath = rel(&cand.object_dir);
        item.codepath_ignore = vec![format!("{}/", cand.carve)];
        item.mainwork = format!(
            "The deterministic test sweep found testgroup \"{label}\" RED.\n\
             Failing at sweep time:\n{failing_lines}\n\
             Test definitions: {tg_file} (declared by the marker file {marker})\nRun logs: {runs_dir}\n\n\
             Make the whole testgroup green:\n\
             1. FIRST reproduce — run: \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\" --broken\n   \
                This asserts the defect is still present. If the group is actually green the item is stale: \
                the engine flags it and you STOP — touch no code.\n\
             2. Diagnose from the run logs, then fix the CODE. Never edit the tests — the {carve}/ \
                subtree is outside your lock scope and belongs to the testwriter.\n\
             3. While iterating, re-run neutrally (no flag): \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\" \
                [--test <id>]. Neutral runs never flag anything.\n\
             4. Completion gate — run: \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\" --fixed\n   \
                This asserts the WHOLE group is green (a fix that breaks a neighbor cannot close the item).\n\n\
             If the fix is comprehensive — spans C4 objects, needs design decisions, won't fit this session — \
             do NOT grind against the timeout and do NOT spawn subagents. Escalate: create a plan work item \
             carrying your full diagnosis:\n  \"$ITER_BIN\" add --project \"$ITER_PROJECT\" --type plan \
             --title \"plan fix for testgroup {label}\" --mainwork \"<your diagnosis and scope>\" --priority {priority}\n\
             then finish this item reporting the escalation.",
            label = group.label,
            failing_lines = failing_lines,
            tg_file = run.tg_file.display(),
            marker = cand.marker_path,
            runs_dir = run.test_dir.join("runs").display(),
            carve = cand.carve.trim_end_matches('/'),
            priority = cand.priority,
        );
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testgroups::TestEntry;

    fn setup(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("iter-sweep-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".iter/.engine")).unwrap();
        let test_dir = root.join("comp/test");
        std::fs::create_dir_all(&test_dir).unwrap();
        (root, test_dir)
    }

    /// The C4 object's marker file, declaring its testgroup.iter.md — the
    /// mandatory key the sweep discovers through.
    fn write_marker(root: &Path, testgroup: &str, test_dir: &str) {
        let mut front = format!("---\nname: \"Comp\"\nlevel: component\ndescription: \"c\"\ntestgroup: {}\n", testgroup);
        if !test_dir.is_empty() {
            front.push_str(&format!("test_dir: {}\n", test_dir));
        }
        front.push_str("---\nbody\n");
        std::fs::write(root.join("comp/comp.marker.iter.md"), front).unwrap();
    }

    fn write_group(test_dir: &Path, label: &str, scripts: &[(&str, &str)], auto_fix: bool) -> PathBuf {
        for (file, body) in scripts {
            std::fs::write(test_dir.join(file), body).unwrap();
        }
        let entries: Vec<TestEntry> = scripts
            .iter()
            .map(|(f, _)| TestEntry { id: f.trim_end_matches(".sh").into(), name: (*f).into(), desc: String::new(), shell: (*f).into() })
            .collect();
        let group = TestGroup { label: label.into(), auto_fix, testlist: entries, ..Default::default() };
        let tg_file = test_dir.join("testgroup.iter.md");
        std::fs::write(&tg_file, testgroups::update("# tests\n", &[group])).unwrap();
        tg_file
    }

    #[test]
    fn red_group_births_one_todo_item_scoped_by_the_marker() {
        let (root, test_dir) = setup("red");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 0\n"), ("t2.sh", "exit 1\n")], false);
        let cfg = Config::default();

        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 1);
        assert_eq!(report.red, 1);
        assert_eq!(report.items_created, 1);
        let queue = Queue::new(&root, &cfg);
        let items = queue.load();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.item_type, "code");
        assert_eq!(item.state, workitems::STATE_TODO, "auto_fix off → todo");
        assert_eq!(item.source, SOURCE);
        assert_eq!(item.source_testgroup, "G");
        assert_eq!(item.source_tests, vec!["t2"]);
        assert_eq!(item.codepath, "comp", "codepath = the marker file's directory");
        assert_eq!(item.codepath_ignore, vec!["test/"]);
        assert!(item.mainwork.contains("--broken"));
        assert!(item.mainwork.contains("--fixed"));
        assert!(item.context.iter().any(|c| c.ends_with("comp.marker.iter.md")), "marker rides along as context");

        // Second sweep: dedup, no duplicate item.
        let report2 = sweep(&root, &cfg);
        assert_eq!(report2.items_created, 0);
        assert_eq!(Queue::new(&root, &cfg).load().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_testgroups_key_means_deliberately_untested() {
        let (root, test_dir) = setup("nokey");
        // Marker WITHOUT a testgroups: key + a testgroup.iter.md sitting right there.
        std::fs::write(root.join("comp/comp.marker.iter.md"), "---\nname: \"Comp\"\nlevel: component\n---\nbody\n").unwrap();
        write_group(&test_dir, "G", &[("t1.sh", "exit 1\n")], false);
        let cfg = Config::default();
        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 0, "undeclared groups never run");
        assert_eq!(report.undeclared, 1);
        assert!(report.notes.iter().any(|n| n.contains("unowned testgroup.iter.md")), "{:?}", report.notes);
        assert!(Queue::new(&root, &cfg).load().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn declared_but_missing_file_is_reported() {
        let (root, _test_dir) = setup("missing");
        write_marker(&root, "test/testgroup.iter.md", "test");
        // No testgroup.iter.md written.
        let cfg = Config::default();
        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 0);
        assert!(report.notes.iter().any(|n| n.contains("does not exist")), "{:?}", report.notes);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pdy_layout_testgroups_beside_marker_scopes_correctly() {
        // PDY-TECH-030: testgroup.iter.md at the C4 object's root, scripts in
        // tests/iter/. The marker's declarations make scoping exact anyway.
        let root = std::env::temp_dir().join(format!("iter-sweep-pdy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".iter/.engine")).unwrap();
        std::fs::create_dir_all(root.join("repos/gateway/tests/iter")).unwrap();
        std::fs::write(
            root.join("repos/gateway/gateway.marker.iter.md"),
            "---\nname: \"Gateway\"\nlevel: container\ntestgroup: testgroup.iter.md\ntest_dir: tests\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(root.join("repos/gateway/tests/iter/t1.sh"), "exit 1\n").unwrap();
        let group = TestGroup {
            label: "G".into(),
            testlist: vec![TestEntry { id: "t1".into(), name: "t1".into(), desc: String::new(), shell: "tests/iter/t1.sh".into() }],
            ..Default::default()
        };
        std::fs::write(root.join("repos/gateway/testgroup.iter.md"), testgroups::update("# tg\n", &[group])).unwrap();
        let cfg = Config::default();
        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 1, "{:?}", report.notes);
        assert_eq!(report.red, 1);
        let items = Queue::new(&root, &cfg).load();
        assert_eq!(items[0].codepath, "repos/gateway", "scoped to the marker file's directory, NOT its parent");
        assert_eq!(items[0].codepath_ignore, vec!["tests/"], "declared test_dir is the carve");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_fix_true_queues_the_item() {
        let (root, test_dir) = setup("autofix");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 1\n")], true);
        let cfg = Config::default();
        sweep(&root, &cfg);
        let items = Queue::new(&root, &cfg).load();
        assert_eq!(items[0].state, workitems::STATE_QUEUED, "auto_fix on → queued");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn error_group_births_testwriter_repair_item() {
        let (root, test_dir) = setup("err");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 9\n")], true);
        let cfg = Config::default();
        let report = sweep(&root, &cfg);
        assert_eq!(report.error, 1);
        let items = Queue::new(&root, &cfg).load();
        assert_eq!(items[0].item_type, "testwriter");
        assert_eq!(items[0].state, workitems::STATE_TODO, "script errors always await review");
        assert_eq!(items[0].codepath, "comp/test", "repair item scoped to the test subtree");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn green_group_auto_closes_unstarted_stale_item() {
        let (root, test_dir) = setup("stale");
        write_marker(&root, "test/testgroup.iter.md", "test");
        let tg = write_group(&test_dir, "G", &[("t1.sh", "exit 1\n")], false);
        let cfg = Config::default();
        sweep(&root, &cfg); // creates the fix item
        // The "bug" gets fixed by other means:
        std::fs::write(test_dir.join("t1.sh"), "exit 0\n").unwrap();
        let report = sweep(&root, &cfg);
        assert_eq!(report.green, 1);
        assert_eq!(report.stale_closed, 1);
        let queue = Queue::new(&root, &cfg);
        assert!(queue.load().is_empty(), "stale item is gone from the open queue");
        let closed = queue.load_closed();
        assert_eq!(closed.len(), 1);
        assert!(closed[0].output.contains("auto-closed by test sweep"));
        // Group block records the green run.
        let groups = testgroups::parse(&std::fs::read_to_string(&tg).unwrap());
        assert!(groups[0].is_green());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fresh_green_group_is_left_alone_and_started_items_are_kept() {
        let (root, test_dir) = setup("fresh");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 0\n")], false);
        let cfg = Config::default();
        sweep(&root, &cfg); // records green
        let report = sweep(&root, &cfg); // fresh green → no run
        assert_eq!(report.ran, 0, "fresh green group must not re-run");

        // A started item (attempts > 0) survives a green sweep.
        let queue = Queue::new(&root, &cfg);
        let mut item = WorkItem { workid: "w-started".into(), item_type: "code".into(), source: SOURCE.into(), source_testgroup: "G".into(), attempts: 1, ..Default::default() };
        item.state = workitems::STATE_QUEUED.into();
        queue.append(&item).unwrap();
        // Force a re-run by making the green stale.
        let tg = test_dir.join("testgroup.iter.md");
        let content = std::fs::read_to_string(&tg).unwrap();
        let mut groups = testgroups::parse(&content);
        groups[0].lastrun = "2020-01-01T00:00:00Z".into();
        std::fs::write(&tg, testgroups::update(&content, &groups)).unwrap();
        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 1);
        assert_eq!(report.stale_closed, 0, "started items are hands-off");
        assert_eq!(queue.load().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unstarted_item_source_tests_refresh_on_new_failures() {
        let (root, test_dir) = setup("refresh");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 1\n"), ("t2.sh", "exit 0\n")], false);
        let cfg = Config::default();
        sweep(&root, &cfg);
        let queue = Queue::new(&root, &cfg);
        assert_eq!(queue.load()[0].source_tests, vec!["t1"]);
        // t2 starts failing too before anyone picks the item up.
        std::fs::write(test_dir.join("t2.sh"), "exit 1\n").unwrap();
        let report = sweep(&root, &cfg);
        assert_eq!(report.items_created, 0);
        assert_eq!(report.items_refreshed, 1);
        assert_eq!(queue.load()[0].source_tests, vec!["t1", "t2"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn locked_object_dir_is_skipped() {
        let (root, test_dir) = setup("locked");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 1\n")], false);
        let cfg = Config::default();
        let comp = root.join("comp").canonicalize().unwrap();
        let lock = locks::acquire_codepath_lock(&comp, "w-busy", "code", 600, &[]).unwrap();
        let report = sweep(&root, &cfg);
        assert_eq!(report.ran, 0, "locked C4 object must be skipped");
        assert!(report.notes.iter().any(|n| n.contains("codepath busy")));
        drop(lock);
        let _ = std::fs::remove_dir_all(&root);
    }
}
