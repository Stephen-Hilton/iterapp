//! End-to-end tests: real `iterloop` binary, real template files, fake claude runner.
//!
//! The fake runner (a shell stub swapped in via ITERLOOP_CLAUDE_BIN) echoes canned
//! `claude -p --output-format json` output, so the whole engine loop — locking,
//! lifecycle, handoff via `iterloop add`, terminal output — runs without burning tokens.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_iterloop");

fn template_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/.iter")
}

/// Build a throwaway project: template .iter, a src/ dir, fast engine timings,
/// and the fake claude stub. Returns (project_root, stub_path).
fn setup_project(name: &str, max_attempts: u32, max_open: usize) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("iterloop-e2e-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    copy_dir(&template_dir(), &root.join(".iter"));
    std::fs::write(root.join("src/thing.txt"), "demo file\n").unwrap();

    std::fs::write(
        root.join(".iter/.engine/config.json"),
        format!(
            r#"{{
  "engine": {{
    "tick_interval_sec": 1,
    "agent_stagger_ms": 10,
    "queue_lock_retry_ms": 20,
    "queue_lock_break_sec": 30,
    "codepath_lock_timeout_sec": 600,
    "codepath_conflict_backoff_sec": 2,
    "max_total_agents": 8,
    "max_open_workitems": {},
    "retry_backoff_sec": 0,
    "max_attempts": {}
  }},
  "globalsettings": {{ "log_level": "info", "log_default_path": "" }}
}}"#,
            max_open, max_attempts
        ),
    )
    .unwrap();

    // The fake claude: finds the project root by walking up from cwd, supports a
    // handoff trigger (calls `iterloop add` like a real agent would) and a failure
    // trigger (non-zero exit).
    let stub = root.join("fake-claude.sh");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
args="$*"
dir="$(pwd)"
while [ "$dir" != "/" ] && [ ! -d "$dir/.iter" ]; do dir="$(dirname "$dir")"; done
case "$args" in
  *HANDOFF_TRIGGER*)
    "$ITERLOOP_BIN" add --project "$dir" --type code --title "handoff child" \
      --mainwork "child work created by handoff" --codepath "./src" \
      --source "agent: plan" >/dev/null 2>&1
    ;;
esac
case "$args" in
  *FAIL_TRIGGER*) echo "stub failure" >&2; exit 1;;
esac
echo '{"type":"result","session_id":"fake-sess-1","result":"fake agent output"}'
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (root, stub)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn seed(root: &Path, line: &str) {
    use std::io::Write;
    let path = root.join(".iter/.engine/workitems.jsonl");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(f, "{}", line).unwrap();
}

fn workitem_json(id: &str, typ: &str, title: &str, codepath: &str, mainwork: &str, prework: &str, postwork: &str) -> String {
    format!(
        r#"{{"workid":"{}","title":"{}","type":"{}","state":"queued","source":"user","priority":5,"risk":1,"codepath":"{}","context":[],"testfiles":[],"prework":[{}],"mainwork":"{}","postwork":[{}],"output":"","attempts":0,"lasterror":"","times":{{"added":"2026-08-11T00:00:00Z","start":"","preworkdone":"","mainworkdone":"","postworkdone":"","closed":""}}}}"#,
        id, title, typ, codepath, prework, mainwork, postwork
    )
}

/// Run the engine with the fake runner until idle; panic (with captured output) on
/// non-zero exit or wall-clock timeout.
fn run_engine(root: &Path, stub: &Path, timeout: Duration) -> String {
    let mut child = Command::new(BIN)
        .args(["run", "--project", root.to_str().unwrap(), "--until-idle"])
        .env("ITERLOOP_CLAUDE_BIN", stub)
        .env("ITERLOOP_BIN", BIN)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("engine spawns");
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                let out = child.wait_with_output().unwrap();
                let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                assert!(status.success(), "engine failed\nstdout:\n{}\nstderr:\n{}", stdout, stderr);
                return stdout;
            }
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let out = child.wait_with_output().unwrap();
                    panic!(
                        "engine timed out after {:?}\nstdout:\n{}",
                        timeout,
                        String::from_utf8_lossy(&out.stdout)
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn closed_items(root: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(root.join(".iter/.engine/workitems_closed.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("closed line parses"))
        .collect()
}

fn open_items(root: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(root.join(".iter/.engine/workitems.jsonl")).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("open line parses"))
        .collect()
}

#[test]
fn full_lifecycle_concurrency_and_handoff() {
    let (root, stub) = setup_project("lifecycle", 3, 200);

    // Three seeds across agent types. The plan item (codepath ".") and the code item
    // (codepath "./src") have overlapping lock scopes, so the codepath lock must
    // serialize them; the handoff trigger makes the plan agent create a 4th item
    // mid-run via `iterloop add`, exactly like a real agent would.
    seed(&root, &workitem_json("e2e-code-1", "code", "code seed", "./src", "implement the thing", r#""git-pull""#, r#""git-commit""#));
    seed(&root, &workitem_json("e2e-plan-1", "plan", "plan seed", ".", "plan the thing HANDOFF_TRIGGER", "", ""));
    seed(&root, &workitem_json("e2e-test-1", "test", "test seed", ".", "run the tests", r#""inline literal prework step""#, ""));

    let stdout = run_engine(&root, &stub, Duration::from_secs(120));

    let open = open_items(&root);
    assert!(open.is_empty(), "queue must drain, still open: {:?}", open);

    let closed = closed_items(&root);
    assert_eq!(closed.len(), 4, "3 seeds + 1 handoff child, got: {:?}", closed.iter().map(|c| c["workid"].as_str().unwrap_or("?")).collect::<Vec<_>>());

    for item in &closed {
        assert_eq!(item["state"], "complete", "item {:?}", item["workid"]);
        assert!(!item["times"]["closed"].as_str().unwrap().is_empty());
        assert!(!item["times"]["start"].as_str().unwrap().is_empty());
        let output = item["output"].as_str().unwrap();
        assert!(output.contains("[mainwork]"), "output must contain mainwork section: {}", output);
        assert!(output.contains("fake agent output"));
    }

    // Prework resolution: file-based step keeps its name; inline literal is labeled inline.
    let code = closed.iter().find(|i| i["workid"] == "e2e-code-1").unwrap();
    let out = code["output"].as_str().unwrap();
    assert!(out.contains("[prework:git-pull]"), "file-based prework label: {}", out);
    assert!(out.contains("[postwork:git-commit]"));
    assert!(!code["times"]["preworkdone"].as_str().unwrap().is_empty());
    assert!(!code["times"]["mainworkdone"].as_str().unwrap().is_empty());
    assert!(!code["times"]["postworkdone"].as_str().unwrap().is_empty());

    let test_item = closed.iter().find(|i| i["workid"] == "e2e-test-1").unwrap();
    assert!(test_item["output"].as_str().unwrap().contains("[prework:inline("));

    // Handoff child: created by the stub through `iterloop add`, picked up and completed.
    let child = closed.iter().find(|i| i["title"] == "handoff child").expect("handoff child must be picked up and closed");
    assert_eq!(child["source"], "agent: plan");
    assert_eq!(child["type"], "code");

    // No lock files left behind.
    assert!(!root.join(".iter.lock").exists());
    assert!(!root.join("src/.iter.lock").exists());

    // The overlapping codepaths must have produced at least one visible requeue OR
    // clean serialization; either way the engine logged lock acquisitions.
    assert!(stdout.contains("codepath lock acquired"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn failure_retries_then_terminal_close() {
    let (root, stub) = setup_project("failure", 2, 200);
    seed(&root, &workitem_json("e2e-fail-1", "code", "always fails", "./src", "do it FAIL_TRIGGER", "", ""));

    run_engine(&root, &stub, Duration::from_secs(60));

    assert!(open_items(&root).is_empty(), "terminal failure must leave the open queue");
    let closed = closed_items(&root);
    assert_eq!(closed.len(), 1);
    let item = &closed[0];
    assert_eq!(item["state"], "failed");
    assert_eq!(item["attempts"], 2, "max_attempts=2 → exactly 2 attempts");
    assert!(item["lasterror"].as_str().unwrap().contains("mainwork"));
    assert!(!item["times"]["closed"].as_str().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_refuses_at_max_open_workitems() {
    let (root, _stub) = setup_project("cap", 3, 1);
    seed(&root, &workitem_json("e2e-existing", "code", "occupies the queue", "./src", "whatever", "", ""));

    let out = Command::new(BIN)
        .args(["add", "--project", root.to_str().unwrap(), "--type", "code", "--title", "one too many", "--mainwork", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "cap refusal must exit 2");
    assert!(String::from_utf8_lossy(&out.stderr).contains("max_open_workitems"));
    assert_eq!(open_items(&root).len(), 1, "nothing appended");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn add_warns_on_unknown_type_but_appends() {
    let (root, _stub) = setup_project("warntype", 3, 200);

    let out = Command::new(BIN)
        .args(["add", "--project", root.to_str().unwrap(), "--type", "bogus", "--title", "odd", "--mainwork", "m"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "warn-at-add must still succeed");
    assert!(String::from_utf8_lossy(&out.stderr).contains("matches no agent"));
    let items = open_items(&root);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["type"], "bogus");
    assert!(!items[0]["workid"].as_str().unwrap().is_empty(), "workid auto-assigned");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn stop_drain_via_cli() {
    let (root, _stub) = setup_project("stop", 3, 200);
    let out = Command::new(BIN)
        .args(["stop", "--project", root.to_str().unwrap(), "--wait"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let signal = std::fs::read_to_string(root.join(".iter/.engine/stop.signal")).unwrap();
    assert!(signal.contains("drain"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn init_copies_template() {
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();

    let out = Command::new(BIN)
        .args(["init", dest.to_str().unwrap(), "--from", template_dir().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(dest.join(".iter/agents/code.md").is_file());
    assert!(dest.join(".iter/.engine/config.json").is_file());
    assert!(dest.join(".iter/prepostwork/git-pull.md").is_file());
    assert!(dest.join(".iter/source/agent.md").is_file());

    // Refuses to overwrite an existing .iter.
    let out2 = Command::new(BIN)
        .args(["init", dest.to_str().unwrap(), "--from", template_dir().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(1));

    let _ = std::fs::remove_dir_all(&dest);
}
