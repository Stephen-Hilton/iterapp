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
/// items get a hard-coded -2 effective-priority boost (lower = sooner), while
/// sweep items must obey the configured priorities (default below user work —
/// numerically ABOVE the default 5 — filling idle time).
pub const SOURCE: &str = "testsweep";

/// One sweep's knobs. These deliberately live on the INVOCATION, not in
/// config.json: the "Test Loop" scheduled workitem carries them as visible
/// `iter testsweep` flags in its command, so editing the schedule's mainwork IS
/// the configuration. A bare `iter testsweep` gets these defaults.
pub struct SweepOptions {
    /// How many testgroups run concurrently.
    pub concurrency: usize,
    /// Priority for fix items born from a group with no green run recorded.
    /// Priorities are lower-is-sooner; sweep defaults sit BELOW the default-5
    /// urgency (numerically above it) so they fill idle capacity.
    pub priority_red: i64,
    /// Priority for fix items born from a group whose green run went stale.
    pub priority_green: i64,
    /// A group with a green run newer than this is left alone.
    pub green_stale_hours: u64,
    /// Wall-clock budget per testgroup run (runtests::run_group).
    pub group_timeout_min: u64,
}

impl Default for SweepOptions {
    fn default() -> Self {
        SweepOptions {
            concurrency: 3,
            priority_red: 6,
            priority_green: 8,
            green_stale_hours: 24,
            group_timeout_min: runtests::DEFAULT_GROUP_TIMEOUT_MIN,
        }
    }
}

/// A missing-tests gap found during discovery: a declared testgroup file that
/// doesn't exist, or a group with an empty testlist. Births a testwriter
/// authoring item in `todo`; `key` is the `source_testgroup` dedup value (the
/// group label, or the declared file path when there is no group yet).
struct AuthoringGap {
    key: String,
    title: String,
    codepath: String,
    context: Vec<String>,
    mainwork: String,
}

/// What kind of file declared a sweepable testgroup (2026-08-17: the sweep
/// universe grew from marker files to markers + use-cases + interfaces). The
/// kind decides fix-item SCOPING: a C4 object's tests scope to its directory;
/// use-case journeys and interface contracts span objects, so their red runs
/// birth fix items scoped to the code root with diagnose-or-escalate guidance.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SweepKind {
    Object,
    Usecase,
    Interface,
}

impl SweepKind {
    /// "marker file" / "use-case file" / "interface file" — for prompts.
    fn declaring(&self) -> &'static str {
        match self {
            SweepKind::Object => "marker file",
            SweepKind::Usecase => "use-case file",
            SweepKind::Interface => "interface file",
        }
    }
}

/// One declaring file the sweep walks: test ownership flows from its
/// `testgroup:` key, never from where a testgroup.iter.md happens to sit.
struct SweepUnit {
    kind: SweepKind,
    name: String,
    /// The declaring file (marker / use-case / interface), for prompts + context.
    declaring_path: String,
    /// The declaring file's directory: testgroup paths resolve against it.
    object_dir: PathBuf,
    testgroup: String,
    test_dir: String,
}

/// One runnable unit: a testgroup declared by a `SweepUnit`, due for a run.
#[derive(Debug)]
struct Candidate {
    kind: SweepKind,
    tg_file: PathBuf,
    group: TestGroup,
    priority: i64,
    /// The declaring file's directory: an Object fix item's codepath / lock scope.
    object_dir: PathBuf,
    /// Subtree carved out of an Object fix item's lock scope (the testwriter's
    /// turf), relative to object_dir, e.g. "test/" or "tests/".
    carve: String,
    /// For the fix-item prompt: which file declared this group.
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
/// `SweepOptions::concurrency`), record every run, and translate results into work
/// items — red → per-group fix item scoped to the C4 object's directory (queued/todo
/// per the group's auto_fix), error → testwriter repair item in todo — with a dedup
/// guard on `source_testgroup` and auto-close of unstarted sweep items whose group
/// came back green. testgroup.iter.md files no marker claims are reported, never run.
pub fn sweep(project_root: &Path, cfg: &Config, opts: &SweepOptions) -> SweepReport {
    let mut report = SweepReport::default();
    let code_root = config::code_root(project_root, cfg);
    let now = chrono::Utc::now();
    let stale_cutoff = now - chrono::Duration::hours(opts.green_stale_hours.max(1) as i64);

    // Same marker universe the Projects view sees (projects.json scan_roots + glob),
    // through the ONE shared resolver (~ expansion included).
    let settings = crate::server::project_settings(project_root);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md").to_string();
    let roots = crate::server::scan_roots(project_root);
    let scan = markers::scan(project_root, &roots, &glob);

    let rel = |p: &Path| {
        p.strip_prefix(&code_root)
            .map(|r| {
                let s = r.to_string_lossy().into_owned();
                if s.is_empty() { ".".to_string() } else { s }
            })
            .unwrap_or_else(|_| p.to_string_lossy().into_owned())
    };
    let mut claimed: Vec<PathBuf> = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    // Missing tests found during discovery (declared testgroup file absent, or a
    // group with an empty testlist): each births ONE testwriter authoring item in
    // `todo` (2026-08-17 flow — the minor-effort "missing tests only" branch;
    // human gate before authoring starts). If the testwriter then finds no code
    // to test (major effort), IT escalates to plan — the deterministic sweep
    // never judges minor vs major.
    let mut authoring: Vec<AuthoringGap> = Vec::new();

    // The sweep universe (2026-08-17): markers AND use-cases AND interfaces.
    // Rules differ per kind on a MISSING `testgroup:` key — for a C4 object,
    // absence is a choice (deliberately untested); for use-cases (E2E journey
    // tests) and interfaces (contract-enforcement tests) tests ARE the point,
    // so a missing key is a coverage gap that births an authoring item. The
    // explicit value `testgroup: none` opts a use-case/interface out.
    let mut units: Vec<SweepUnit> = Vec::new();
    for node in &scan.nodes {
        if node.testgroup.is_empty() {
            report.undeclared += 1;
            continue;
        }
        units.push(SweepUnit {
            kind: SweepKind::Object,
            name: node.name.clone(),
            declaring_path: node.path.clone(),
            object_dir: PathBuf::from(&node.dir),
            testgroup: node.testgroup.clone(),
            test_dir: node.test_dir.clone(),
        });
    }
    let mut keyless: Vec<(SweepKind, String, String)> = Vec::new(); // (kind, name, declaring file)
    for uc in &scan.usecases {
        let dir = Path::new(&uc.file).parent().map(PathBuf::from).unwrap_or_else(|| code_root.clone());
        match uc.testgroup.trim() {
            "none" => report.undeclared += 1,
            "" => keyless.push((SweepKind::Usecase, uc.name.clone(), uc.file.clone())),
            tg => units.push(SweepUnit {
                kind: SweepKind::Usecase,
                name: uc.name.clone(),
                declaring_path: uc.file.clone(),
                object_dir: dir,
                testgroup: tg.to_string(),
                test_dir: uc.test_dir.clone(),
            }),
        }
    }
    for iface in &scan.interfaces {
        let dir = Path::new(&iface.file).parent().map(PathBuf::from).unwrap_or_else(|| code_root.clone());
        match iface.testgroup.trim() {
            "none" => report.undeclared += 1,
            "" => keyless.push((SweepKind::Interface, iface.id.clone(), iface.file.clone())),
            tg => units.push(SweepUnit {
                kind: SweepKind::Interface,
                name: iface.id.clone(),
                declaring_path: iface.file.clone(),
                object_dir: dir,
                testgroup: tg.to_string(),
                test_dir: iface.test_dir.clone(),
            }),
        }
    }

    // Use-cases/interfaces with NO `testgroup:` key: the coverage gap itself is
    // the finding. One authoring item each — the testwriter creates the
    // testgroup file, adds the key to the declaring file (sanctioned write),
    // and authors the tests.
    for (kind, name, declaring) in keyless {
        let flavor = match kind {
            SweepKind::Usecase =>
                "end-to-end JOURNEY tests: scripts that walk the actual user journey this use-case \
                 describes, through the real participants",
            _ =>
                "CONTRACT-enforcement tests: scripts that assert the real providers' inputs/outputs \
                 against the contract's example in the interface file body — so drift turns red \
                 instead of silently accumulating",
        };
        report.notes.push(format!(
            "{} \"{}\" has no testgroup: key — testwriter authoring item ensured",
            kind.declaring(),
            name
        ));
        authoring.push(AuthoringGap {
            key: rel(Path::new(&declaring)),
            title: format!("author {} tests for \"{}\" (no testgroup declared)",
                if kind == SweepKind::Usecase { "E2E" } else { "contract" }, name),
            codepath: rel(Path::new(&declaring).parent().unwrap_or(&code_root)),
            context: vec![declaring.clone()],
            mainwork: format!(
                "The {declaring_kind} {declaring} declares no `testgroup:` key — this {what} has no tests.\n\
                 Author {flavor}.\n\
                 1. Create a testgroup.iter.md near the declaring file, add `testgroup: <relative path>` \
                 (and `test_dir:`) to the declaring file's frontmatter — this registration is your one \
                 sanctioned write outside the test directory — then write and register the scripts \
                 (shell-script contract: exit 0 as-expected / 1 unexpected / other = script error; \
                 last line `ITER_RESULT pass=X fail=Y total=Z`).\n\
                 2. If the underlying capability doesn't exist yet to test, escalate instead: \
                 \"$ITER_BIN\" add --project \"$ITER_PROJECT\" --type plan \
                 --title \"plan: build out {name}\" --source-testgroup \"{key}\" \
                 --mainwork \"<your gap analysis>\" — then finish this item reporting the escalation.\n\
                 3. If this {what} genuinely should not be tested, set `testgroup: none` on the \
                 declaring file and report why.",
                declaring_kind = kind.declaring(),
                declaring = declaring,
                what = if kind == SweepKind::Usecase { "use-case" } else { "interface" },
                flavor = flavor,
                name = name,
                key = rel(Path::new(&declaring)),
            ),
        });
    }

    for unit in &units {
        let object_dir = &unit.object_dir;
        let tg_file = object_dir.join(&unit.testgroup);
        let tg_file = match tg_file.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                report.notes.push(format!(
                    "\"{}\": {} declares testgroup: {} but the file does not exist — testwriter authoring item ensured",
                    unit.name,
                    unit.kind.declaring(),
                    unit.testgroup
                ));
                // The declared test dir may not exist yet either, so the item is
                // scoped to the declaring file's directory; the prompt confines writes.
                let declared = rel(&object_dir.join(&unit.testgroup));
                authoring.push(AuthoringGap {
                    key: declared.clone(),
                    title: format!("author tests for \"{}\" (declared testgroup file missing)", unit.name),
                    codepath: rel(object_dir),
                    context: vec![unit.declaring_path.clone()],
                    mainwork: format!(
                        "The {declaring_kind} {marker} declares `testgroup: {tg}` but that file does not exist.\n\
                         Create it, and author the tests it should register:\n\
                         1. Read the declaring file, the local bizreq/techreq (and the global ones in $ITER_REQS), and the code.\n\
                         2. If the CODE this should exercise is missing too, this is a plan-sized gap, not a testwriter task: \
                         escalate — create a plan work item carrying what you found (\"$ITER_BIN\" add --project \"$ITER_PROJECT\" \
                         --type plan --title \"plan: build out {name}\" --source-testgroup \"{declared}\" \
                         --mainwork \"<gap analysis>\") and finish this item reporting the escalation.\n\
                         3. Otherwise create {declared} with testgroups, write the test scripts (shell-script contract: \
                         exit 0 as-expected / 1 unexpected / other = script error; last line `ITER_RESULT pass=X fail=Y total=Z`), \
                         and register every test in the testlist. Confine writes to the test directory.",
                        declaring_kind = unit.kind.declaring(),
                        marker = unit.declaring_path,
                        tg = unit.testgroup,
                        name = unit.name,
                        declared = declared,
                    ),
                });
                continue;
            }
        };
        claimed.push(tg_file.clone());
        // The carve: declared test_dir, else the declared testgroups file's own
        // subdirectory (when it has one), else the global default.
        let carve = if !unit.test_dir.is_empty() {
            unit.test_dir.trim_end_matches('/').to_string()
        } else {
            tg_file
                .parent()
                .and_then(|p| p.strip_prefix(object_dir).ok())
                .map(|r| r.to_string_lossy().into_owned())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| cfg.globalsettings.test_dir.clone())
        };
        let Ok(content) = std::fs::read_to_string(&tg_file) else { continue };
        for group in testgroups::parse(&content) {
            report.examined += 1;
            if group.testlist.is_empty() {
                // Tests not written yet is never a red run — it becomes a
                // testwriter authoring item (todo) instead.
                report.notes.push(format!("\"{}\": no tests registered — testwriter authoring item ensured", group.label));
                authoring.push(AuthoringGap {
                    key: group.label.clone(),
                    title: format!("author tests for testgroup \"{}\" (empty testlist)", group.label),
                    codepath: rel(tg_file.parent().unwrap_or(object_dir)),
                    context: vec![tg_file.to_string_lossy().into_owned(), unit.declaring_path.clone()],
                    mainwork: format!(
                        "Testgroup \"{label}\" in {tg_file} has no tests registered.\n\
                         Author them: read the {declaring_kind} {marker}, the local bizreq/techreq (and the global ones in \
                         $ITER_REQS), and the code; write test scripts honoring the shell-script contract (exit 0 \
                         as-expected / 1 unexpected / other = script error; last line `ITER_RESULT pass=X fail=Y total=Z`) \
                         and register each in the group's testlist. If the code this group should exercise does not \
                         exist yet, escalate to a plan work item (`iter add --type plan --source-testgroup \"{label}\" …`) \
                         with your gap analysis instead of writing tests against nothing, and finish this item \
                         reporting the escalation.",
                        label = group.label,
                        tg_file = tg_file.display(),
                        declaring_kind = unit.kind.declaring(),
                        marker = unit.declaring_path,
                    ),
                });
                continue;
            }
            let fresh_green = group.is_green()
                && workitems::parse_iso(&group.lastrun).map(|t| t > stale_cutoff).unwrap_or(false);
            if fresh_green {
                continue;
            }
            let priority = if group.is_green() {
                opts.priority_green // was green, went stale
            } else {
                opts.priority_red // never/no-longer green
            };
            // An agent working anywhere near the tests makes results meaningless
            // (and a testwriter may be mid-edit on the scripts): skip until quiet.
            if let Some(lock) = locks::find_active_lock(object_dir, &[], now) {
                report.notes.push(format!("skip \"{}\": codepath busy ({})", group.label, lock.display()));
                continue;
            }
            candidates.push(Candidate {
                kind: unit.kind,
                tg_file: tg_file.clone(),
                group,
                priority,
                object_dir: object_dir.clone(),
                carve: carve.clone(),
                marker_path: unit.declaring_path.clone(),
            });
        }
    }

    // testgroup.iter.md files no declaring file claims: surfaced, never run.
    for file in testgroups::find_files(&code_root) {
        let canon = file.canonicalize().unwrap_or(file.clone());
        if !claimed.contains(&canon) {
            report.notes.push(format!(
                "unowned testgroup.iter.md (no marker/use-case/interface file declares it via testgroup:): {}",
                file.display()
            ));
        }
    }

    // Run candidates through the deterministic runner, a few groups at a time.
    let work: Arc<Mutex<Vec<Candidate>>> = Arc::new(Mutex::new(candidates));
    let results: Arc<Mutex<Vec<(Candidate, Result<GroupRunResult, String>)>>> = Arc::new(Mutex::new(Vec::new()));
    let workers = opts.concurrency.max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let work = Arc::clone(&work);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let next = work.lock().unwrap().pop();
                let Some(cand) = next else { break };
                let run = runtests::run_group(&cand.tg_file, &cand.group.label, None, opts.group_timeout_min);
                results.lock().unwrap().push((cand, run));
            });
        }
    });
    let results = Arc::try_unwrap(results).expect("workers joined").into_inner().unwrap();

    // Translate results into queue actions.
    let queue = Queue::new(project_root, cfg);

    // Authoring gaps first: one open testwriter item per gap (same
    // source_testgroup dedup as fix items — an open item, whoever created it,
    // suppresses a new one). Born `todo`: new-test authoring gets a human gate.
    for gap in &authoring {
        let open_count = queue.load().len();
        let mut item = WorkItem {
            workid: uuid::Uuid::new_v4().to_string(),
            item_type: "testwriter".into(),
            state: workitems::STATE_TODO.into(),
            title: gap.title.clone(),
            source: SOURCE.into(),
            priority: opts.priority_red,
            codepath: gap.codepath.clone(),
            source_testgroup: gap.key.clone(),
            context: gap.context.clone(),
            mainwork: gap.mainwork.clone(),
            ..Default::default()
        };
        item.times.added = workitems::now_iso();
        let created = queue.with_lock(|items| {
            if items.iter().any(|i| i.source_testgroup == gap.key) {
                return false;
            }
            if open_count >= cfg.engine.max_open_workitems {
                return false; // full queue: suppressed, next sweep retries
            }
            items.push(item.clone());
            true
        });
        match created {
            Ok(true) => {
                report.items_created += 1;
                logging::info(
                    "sweep",
                    &format!("created testwriter authoring item {} (\"{}\", todo)", item.workid, gap.key),
                );
            }
            Ok(false) => {}
            Err(e) => report.notes.push(format!("\"{}\": {}", gap.key, e)),
        }
    }

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
             (declared by the {declaring_kind} {marker}). Verify with: \
             \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\"",
            label = group.label,
            failing_lines = failing_lines,
            tg_file = run.tg_file.display(),
            declaring_kind = cand.kind.declaring(),
            marker = cand.marker_path,
        );
    } else if cand.kind == SweepKind::Object {
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
             --title \"plan fix for testgroup {label}\" --source-testgroup \"{label}\" \
             --mainwork \"<your diagnosis and scope>\" --priority {priority}\n\
             then finish this item reporting the escalation.",
            label = group.label,
            failing_lines = failing_lines,
            tg_file = run.tg_file.display(),
            marker = cand.marker_path,
            runs_dir = run.test_dir.join("runs").display(),
            carve = cand.carve.trim_end_matches('/'),
            priority = cand.priority,
        );
    } else {
        // Use-case journey / interface contract red: the failure spans C4
        // objects, so the fix can't pre-scope to one directory. The item takes
        // the whole code root as its lock scope (heavy — which is why auto_fix
        // defaults false and this usually lands in todo, where a human can
        // narrow the codepath before queueing) and the prompt leads with
        // diagnosis: fix locally when the culprit is small, escalate to plan
        // when it's structural.
        let what = if cand.kind == SweepKind::Usecase { "use-case journey" } else { "interface contract" };
        item.item_type = "code".into();
        item.state = if group.auto_fix { workitems::STATE_QUEUED.into() } else { workitems::STATE_TODO.into() };
        item.title = format!("fix red {} testgroup \"{}\" ({} failing)", what, group.label, failing.len());
        item.codepath = ".".into();
        item.mainwork = format!(
            "The deterministic test sweep found the {what} testgroup \"{label}\" RED.\n\
             Failing at sweep time:\n{failing_lines}\n\
             Test definitions: {tg_file} (declared by the {declaring_kind} {marker})\nRun logs: {runs_dir}\n\n\
             This failure spans C4 objects, so your lock scope is the whole code root — work surgically:\n\
             1. FIRST reproduce — run: \"$ITER_BIN\" runtests --project \"$ITER_PROJECT\" --group \"{label}\" --broken\n   \
                If the group is actually green the item is stale: the engine flags it and you STOP.\n\
             2. Diagnose from the run logs WHICH object(s) are at fault ({context_hint}).\n\
             3. Small, local culprit → fix the CODE there (never the tests), re-run neutrally while iterating, \
                then gate completion with --fixed.\n\
             4. Structural culprit (contract change rippling through providers, journey needs new capability) → \
                do NOT grind: escalate — \"$ITER_BIN\" add --project \"$ITER_PROJECT\" --type plan \
                --title \"plan fix for {what} {label}\" --source-testgroup \"{label}\" \
                --mainwork \"<your diagnosis: objects at fault, contract/journey deltas>\" --priority {priority} \
                — then finish this item reporting the escalation.",
            what = what,
            label = group.label,
            failing_lines = failing_lines,
            tg_file = run.tg_file.display(),
            declaring_kind = cand.kind.declaring(),
            marker = cand.marker_path,
            runs_dir = run.test_dir.join("runs").display(),
            context_hint = if cand.kind == SweepKind::Usecase {
                "the use-case file's participants: list names them in journey order"
            } else {
                "the interface file's contract body is the expected shape; check its providers"
            },
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
    fn usecase_without_testgroup_key_births_authoring_item_and_none_opts_out() {
        let (root, _td) = setup("ucswp");
        std::fs::create_dir_all(root.join("usecases")).unwrap();
        std::fs::write(
            root.join("usecases/login.usecase.iter.md"),
            "---\nname: \"User Login\"\ndescription: \"login journey\"\n---\nbody\n",
        )
        .unwrap();
        // An interface deliberately opted out contributes nothing.
        std::fs::create_dir_all(root.join("interfaces")).unwrap();
        std::fs::write(
            root.join("interfaces/auth.interface.iter.md"),
            "---\ninterface: auth-api\nkind: http\ntestgroup: none\n---\ncontract\n",
        )
        .unwrap();
        let cfg = Config::default();
        let report = sweep(&root, &cfg, &SweepOptions::default());
        let items = Queue::new(&root, &cfg).load();
        assert_eq!(items.len(), 1, "notes: {:?}", report.notes);
        assert_eq!(items[0].item_type, "testwriter");
        assert_eq!(items[0].state, workitems::STATE_TODO);
        assert!(items[0].title.contains("E2E"), "{}", items[0].title);
        // Dedup on a second sweep.
        sweep(&root, &cfg, &SweepOptions::default());
        assert_eq!(Queue::new(&root, &cfg).load().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn red_usecase_group_scopes_fix_item_to_code_root() {
        let (root, _td) = setup("ucred");
        let uc_test = root.join("usecases/login-test");
        std::fs::create_dir_all(&uc_test).unwrap();
        std::fs::write(
            root.join("usecases/login.usecase.iter.md"),
            "---\nname: \"User Login\"\ntestgroup: login-test/testgroup.iter.md\ntest_dir: login-test\n---\nbody\n",
        )
        .unwrap();
        write_group(&uc_test, "Login E2E", &[("t1.sh", "exit 1\n")], false);
        let cfg = Config::default();
        let report = sweep(&root, &cfg, &SweepOptions::default());
        assert_eq!(report.red, 1, "notes: {:?}", report.notes);
        let items = Queue::new(&root, &cfg).load();
        let fix = items.iter().find(|i| i.item_type == "code").expect("fix item born");
        assert_eq!(fix.codepath, ".", "journey failures span objects → code-root scope");
        assert_eq!(fix.state, workitems::STATE_TODO);
        assert!(fix.mainwork.contains("use-case journey"), "{}", fix.mainwork);
        assert!(fix.mainwork.contains("--source-testgroup"), "escalation carries provenance");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_tests_birth_testwriter_authoring_items_in_todo() {
        // Declared testgroup file absent → authoring item keyed by the declared path.
        let (root, _test_dir) = setup("authoring");
        write_marker(&root, "test/testgroup.iter.md", "test");
        std::fs::remove_dir_all(root.join("comp/test")).unwrap();
        let cfg = Config::default();
        let report = sweep(&root, &cfg, &SweepOptions::default());
        assert_eq!(report.items_created, 1);
        let queue = Queue::new(&root, &cfg);
        let items = queue.load();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_type, "testwriter");
        assert_eq!(items[0].state, workitems::STATE_TODO, "authoring gets a human gate");
        assert_eq!(items[0].source, SOURCE);
        assert!(items[0].mainwork.contains("escalate"), "major-effort branch delegated to the agent");
        // Dedup: a second sweep creates nothing new.
        let report2 = sweep(&root, &cfg, &SweepOptions::default());
        assert_eq!(report2.items_created, 0);
        assert_eq!(queue.load().len(), 1);

        // Empty testlist → authoring item keyed by the group label.
        let (root2, test_dir2) = setup("authoring-empty");
        write_marker(&root2, "test/testgroup.iter.md", "test");
        let group = TestGroup { label: "Empty".into(), ..Default::default() };
        std::fs::write(test_dir2.join("testgroup.iter.md"), testgroups::update("# tests\n", &[group])).unwrap();
        let report3 = sweep(&root2, &cfg, &SweepOptions::default());
        assert_eq!(report3.items_created, 1);
        let items2 = Queue::new(&root2, &cfg).load();
        assert_eq!(items2[0].source_testgroup, "Empty");
        assert_eq!(items2[0].state, workitems::STATE_TODO);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&root2);
    }

    #[test]
    fn red_group_births_one_todo_item_scoped_by_the_marker() {
        let (root, test_dir) = setup("red");
        write_marker(&root, "test/testgroup.iter.md", "test");
        write_group(&test_dir, "G", &[("t1.sh", "exit 0\n"), ("t2.sh", "exit 1\n")], false);
        let cfg = Config::default();

        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        let report2 = sweep(&root, &cfg, &SweepOptions::default());
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        sweep(&root, &cfg, &SweepOptions::default());
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        sweep(&root, &cfg, &SweepOptions::default()); // creates the fix item
        // The "bug" gets fixed by other means:
        std::fs::write(test_dir.join("t1.sh"), "exit 0\n").unwrap();
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        sweep(&root, &cfg, &SweepOptions::default()); // records green
        let report = sweep(&root, &cfg, &SweepOptions::default()); // fresh green → no run
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        sweep(&root, &cfg, &SweepOptions::default());
        let queue = Queue::new(&root, &cfg);
        assert_eq!(queue.load()[0].source_tests, vec!["t1"]);
        // t2 starts failing too before anyone picks the item up.
        std::fs::write(test_dir.join("t2.sh"), "exit 1\n").unwrap();
        let report = sweep(&root, &cfg, &SweepOptions::default());
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
        let report = sweep(&root, &cfg, &SweepOptions::default());
        assert_eq!(report.ran, 0, "locked C4 object must be skipped");
        assert!(report.notes.iter().any(|n| n.contains("codepath busy")));
        drop(lock);
        let _ = std::fs::remove_dir_all(&root);
    }
}
