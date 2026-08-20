//! End-to-end tests: real `iter` binary, real template files, fake claude runner.
//!
//! The fake runner (a shell stub swapped in via ITER_CLAUDE_BIN) echoes canned
//! `claude -p --output-format json` output, so the whole engine loop — locking,
//! lifecycle, handoff via `iter add`, terminal output — runs without burning tokens.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_iter");

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

    // The fake claude: relies on the ITER_BIN/ITER_PROJECT env vars the ENGINE
    // injects into every agent session — exactly like the real agent templates —
    // so this test proves the injection end-to-end. Also supports a failure trigger.
    let stub = root.join("fake-claude.sh");
    std::fs::write(
        &stub,
        r#"#!/bin/sh
args="$*"
case "$args" in
  *HANDOFF_TRIGGER*)
    "$ITER_BIN" add --project "$ITER_PROJECT" --type code --title "handoff child" \
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
        .env("ITER_CLAUDE_BIN", stub)
        // Isolate from the developer machine: ~ resolves inside the test root, so the
        // REAL ~/.claude/iter-usage-snapshot.json (which may show 95%+ while other
        // iter projects run on this box) can't throttle the test engine to 0 agents.
        .env("HOME", root)
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
    // mid-run via `iter add`, exactly like a real agent would.
    seed(&root, &workitem_json("e2e-code-1", "code", "code seed", "./src", "implement the thing", r#""git-pull""#, r#""git-commit""#));
    seed(&root, &workitem_json("e2e-plan-1", "plan", "plan seed", ".", "plan the thing HANDOFF_TRIGGER", "", ""));
    // (type "testwriter": the old test agent is retired — the deterministic sweep runs tests.)
    seed(&root, &workitem_json("e2e-test-1", "testwriter", "testwriter seed", ".", "write the tests", r#""inline literal prework step""#, ""));

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

    // Handoff child: created by the stub through `iter add`, picked up and completed.
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
fn init_embedded_template_and_heal() {
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();

    // No --from, no env: the template embedded in the binary scaffolds everything.
    let out = Command::new(BIN).args(["init", dest.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(dest.join(".iter/agents/code.md").is_file());
    assert!(dest.join(".iter/.engine/config.json").is_file());
    assert!(dest.join(".iter/prepostwork/git-pull.md").is_file());
    assert!(dest.join(".iter/source/agent.md").is_file());

    // Idempotent + healing: user edits survive, deleted files come back.
    std::fs::write(dest.join(".iter/agents/code.md"), "user customization").unwrap();
    std::fs::remove_file(dest.join(".iter/source/error.md")).unwrap();
    let out2 = Command::new(BIN).args(["init", dest.to_str().unwrap()]).output().unwrap();
    assert!(out2.status.success());
    assert!(String::from_utf8_lossy(&out2.stdout).contains("1 file(s) added"));
    assert_eq!(
        std::fs::read_to_string(dest.join(".iter/agents/code.md")).unwrap(),
        "user customization"
    );
    assert!(dest.join(".iter/source/error.md").is_file());

    // --from a directory still works, same add-missing-only semantics.
    let out3 = Command::new(BIN)
        .args(["init", dest.to_str().unwrap(), "--from", template_dir().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out3.status.success());
    assert_eq!(
        std::fs::read_to_string(dest.join(".iter/agents/code.md")).unwrap(),
        "user customization",
        "--from must not overwrite either"
    );

    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn start_serves_webapp_and_runs_engine() {
    use std::io::{Read, Write};
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(dest.join(".iter/.engine")).unwrap();
    // Fast ticks so the stop signal is honored quickly (heal fills the other files).
    std::fs::write(
        dest.join(".iter/.engine/config.json"),
        r#"{"engine":{"tick_interval_sec":1},"globalsettings":{"log_default_path":""}}"#,
    )
    .unwrap();

    let port = 21000 + (std::process::id() % 20000) as u16;
    let mut child = Command::new(BIN)
        .args(["start", "--project", dest.to_str().unwrap(), "--port", &port.to_string()])
        .env("HOME", &dest) // isolate ~ (usage snapshot, server registry) from the real machine
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // The webapp must answer with the embedded page.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut sock) => {
                sock.write_all(b"GET / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").unwrap();
                let mut resp = String::new();
                let _ = sock.read_to_string(&mut resp);
                assert!(resp.starts_with("HTTP/1.1 200"), "bad response: {}", &resp[..resp.len().min(120)]);
                assert!(resp.contains("IterLoop"), "page body must be the embedded webapp");
                break;
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "webapp never came up on port {}", port);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // Stop the ENGINE via the signal: the loop exits but the webapp stays up
    // (pause/resume from the browser depends on this), state reads "stopped".
    std::thread::sleep(Duration::from_millis(1200)); // let the scheduler clear old signals first
    let ok = Command::new(BIN).args(["stop", "--project", dest.to_str().unwrap()]).status().unwrap();
    assert!(ok.success());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = http(port, "GET", "/api/state", None);
        if state.contains("\"engine\":\"stopped\"") {
            break;
        }
        assert!(Instant::now() < deadline, "engine never reached stopped: {}", state);
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(child.try_wait().unwrap().is_none(), "server must outlive the engine loop");

    // Resume from the API, then shut the whole process down.
    let resumed = http(port, "POST", "/api/engine", Some(r#"{"action":"resume"}"#));
    assert!(resumed.contains("running"), "resume must restart the loop: {}", resumed);
    let _ = http(port, "POST", "/api/engine", Some(r#"{"action":"shutdown"}"#));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "shutdown action did not exit the process");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success());
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("iterapp webapp:"), "must print the URL: {}", stdout);
    assert!(stdout.contains(&format!("localhost:{}/", port)));

    let _ = std::fs::remove_dir_all(&dest);
}

/// Minimal HTTP client for the API tests: one request, connection-close semantics.
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> String {
    use std::io::{Read, Write};
    let Ok(mut sock) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return String::new();
    };
    let body = body.unwrap_or("");
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method, path, body.len(), body
    );
    let _ = sock.write_all(req.as_bytes());
    let mut resp = String::new();
    let _ = sock.read_to_string(&mut resp);
    resp
}

#[test]
fn api_crud_history_markers_and_settings() {
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-api-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(dest.join(".iter/.engine")).unwrap();
    std::fs::write(
        dest.join(".iter/.engine/config.json"),
        r#"{"engine":{"tick_interval_sec":1,"max_open_workitems":3},"globalsettings":{"log_default_path":""}}"#,
    )
    .unwrap();
    // An iter-file tree: node, interface, plain doc — roles come from the FILENAMES.
    std::fs::create_dir_all(dest.join("svc")).unwrap();
    std::fs::write(dest.join("root.marker.iter.md"), "---\nname: API Test Project\nlevel: project\n---\n").unwrap();
    std::fs::write(
        dest.join("svc/svc.marker.iter.md"),
        "---\nname: Svc\nlevel: component\nuses: [pay-api]\nprovides: [svc-api]\n---\ncontext",
    )
    .unwrap();
    std::fs::write(
        dest.join("svc/svc-api.interface.iter.md"),
        "---\ninterface: svc-api\nkind: http\nendpoint: GET /svc\n---\ncontract",
    )
    .unwrap();
    std::fs::write(dest.join("svc/bizreq.iter.md"), "plain context\n").unwrap();

    let port = 22000 + (std::process::id() % 20000) as u16;
    let mut child = Command::new(BIN)
        .args(["start", "--project", dest.to_str().unwrap(), "--port", &port.to_string()])
        .env("ITER_CLAUDE_BIN", "/usr/bin/false") // any picked-up item fails fast, no real claude
        .env("HOME", &dest) // isolate ~ (usage snapshot, server registry) from the real machine
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "server never came up");
        std::thread::sleep(Duration::from_millis(100));
    }

    // meta
    let meta = http(port, "GET", "/api/meta", None);
    assert!(meta.contains("\"prepostwork\""), "{}", meta);
    assert!(meta.contains("git-pull"));

    // create (todo so the engine leaves it alone), list, patch, actions
    let created = http(port, "POST", "/api/workitems", Some(r#"{"type":"code","title":"t1","mainwork":"m1","state":"todo"}"#));
    assert!(created.contains("201") && created.contains("workid"), "{}", created);
    let created2 = http(port, "POST", "/api/workitems", Some(r#"{"type":"bogus","title":"t2","mainwork":"m2","state":"todo"}"#));
    assert!(created2.contains("matches no agent"), "warn-at-add over the API: {}", created2);
    let list = http(port, "GET", "/api/workitems", None);
    let body = list.split("\r\n\r\n").nth(1).unwrap_or("");
    let parsed: serde_json::Value = serde_json::from_str(body).expect("list parses");
    let open = parsed["open"].as_array().unwrap();
    assert_eq!(open.len(), 2);
    let id = open[0]["workid"].as_str().unwrap().to_string();

    let patched = http(port, "PATCH", &format!("/api/workitems/{}", id), Some(r#"{"title":"renamed","priority":9}"#));
    assert!(patched.contains("renamed"), "{}", patched);
    let acted = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"complete"}"#));
    assert!(acted.contains("complete"), "{}", acted);
    let third = http(port, "POST", "/api/workitems", Some(r#"{"type":"code","title":"t3","mainwork":"m3","state":"todo"}"#));
    assert!(third.contains("201"), "{}", third);
    // cap is 3: two open + one more = refusal
    let _fourth_a = http(port, "POST", "/api/workitems", Some(r#"{"type":"code","title":"t4","mainwork":"m4","state":"todo"}"#));
    let refused = http(port, "POST", "/api/workitems", Some(r#"{"type":"code","title":"t5","mainwork":"m5","state":"todo"}"#));
    assert!(refused.contains("409") && refused.contains("max_open_workitems"), "{}", refused);

    // history includes the completed item today
    let hist = http(port, "GET", "/api/history?days=7", None);
    assert!(hist.contains("\"days\""), "{}", hist);
    assert!(hist.contains("\"complete\":1"), "today's bucket counts the completion: {}", hist);

    // markers: node + interface + plain sorted by frontmatter role
    let markers = http(port, "GET", "/api/markers", None);
    assert!(markers.contains("API Test Project"), "{}", markers);
    assert!(markers.contains("\"svc-api\""));
    assert!(markers.contains("bizreq.iter.md"));

    // usecase create lands in globalsettings.usecase_default_path
    // (default {codepath}/usecases/), named <slug>.usecase.iter.md
    let uc = http(port, "POST", "/api/usecases", Some(r#"{"name":"Pay Flow","description":"d","participants":["1 svc"]}"#));
    assert!(uc.contains("201"), "{}", uc);
    assert!(
        dest.join("usecases/pay-flow.usecase.iter.md").is_file(),
        "created use-case must land under <code_root>/usecases/"
    );

    // config roundtrip
    let put = http(port, "PUT", "/api/config", Some(r#"{"engine":{"tick_interval_sec":2},"globalsettings":{}}"#));
    assert!(put.contains("200"), "{}", put);
    let got = http(port, "GET", "/api/config", None);
    assert!(got.contains("\"tick_interval_sec\": 2") || got.contains("\"tick_interval_sec\":2"), "{}", got);

    // projectsettings roundtrip
    let ps = http(port, "PUT", "/api/projectsettings", Some(r#"{"project_name":"Renamed","url_slug":"api-test"}"#));
    assert!(ps.contains("Renamed"), "{}", ps);
    let ps2 = http(port, "GET", "/api/projectsettings", None);
    assert!(ps2.contains("api-test") && ps2.contains("marker_glob"), "defaults overlay: {}", ps2);

    // agents: list, edit roundtrip (body + comments untouched), bad names rejected
    let ag = http(port, "GET", "/api/agents", None);
    assert!(ag.contains("\"type\":\"code\""), "{}", ag);
    let ag_put = http(
        port,
        "PUT",
        "/api/agents/code",
        Some(r#"{"model":"sonnet","max_agent_count":5,"visible":false,"description":"edited via api"}"#),
    );
    assert!(ag_put.contains("200") && ag_put.contains("sonnet"), "{}", ag_put);
    let agent_md = std::fs::read_to_string(dest.join(".iter/agents/code.md")).unwrap();
    assert!(agent_md.contains("model: sonnet") && agent_md.contains("max_agent_count: 5"), "{}", agent_md);
    assert!(agent_md.contains("You are the **code** agent"), "body must be untouched: {}", agent_md);
    let ag_missing = http(port, "PUT", "/api/agents/nope", Some(r#"{"model":"opus"}"#));
    assert!(ag_missing.contains("404"), "{}", ag_missing);
    let ag_bad = http(port, "PUT", "/api/agents/..%2Fescape", Some(r#"{"model":"opus"}"#));
    assert!(ag_bad.contains("400"), "{}", ag_bad);
    let ag_field = http(port, "PUT", "/api/agents/code", Some(r#"{"nonsense_key":"x"}"#));
    assert!(ag_field.contains("400"), "unknown fields must be rejected: {}", ag_field);

    // servers registry includes us
    let servers = http(port, "GET", "/api/servers", None);
    assert!(servers.contains(&format!("\"port\":{}", port)) || servers.contains(&format!("\"port\": {}", port)), "{}", servers);

    let _ = http(port, "POST", "/api/engine", Some(r#"{"action":"shutdown"}"#));
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "shutdown did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn copy_binary_and_run_scaffolds_project() {
    // The deployment story: an empty directory, `iter run` — the .iter tree
    // appears from the embedded template and the engine starts clean.
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-scaffold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).unwrap();

    let out = Command::new(BIN)
        .args(["run", "--project", dest.to_str().unwrap(), "--until-idle"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("missing .iter file"), "run must report the scaffold: {}", stdout);
    assert!(dest.join(".iter/agents/plan.md").is_file());
    assert!(dest.join(".iter/.engine/workitems.jsonl").is_file());

    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn critreview_success_abort_and_retry_paths() {
    let (root, _stub) = setup_project("critrev", 1, 10);
    let material = root.join("plan-to-review.md");
    std::fs::write(&material, "1. build the thing\n2. test the thing\n").unwrap();

    let write_stub = |name: &str, body: &str| -> PathBuf {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    };
    let run = |stub: &Path| {
        Command::new(BIN)
            .args(["critreview", "--project", root.to_str().unwrap(), "--file", material.to_str().unwrap()])
            .env("ITER_CLAUDE_BIN", stub)
            .env("ITER_WORKID", "wi-critrev-1")
            .output()
            .unwrap()
    };
    let flag_path = root.join(".iter/.engine/critfail-wi-critrev-1.txt");
    let take_flag = || {
        let text = std::fs::read_to_string(&flag_path).ok();
        let _ = std::fs::remove_file(&flag_path);
        text
    };

    // Exit 0: critic returns a review; it lands on stdout and spend is recorded.
    let ok = write_stub(
        "critic-ok.sh",
        "#!/bin/sh\necho '{\"type\":\"result\",\"session_id\":\"c1\",\"result\":\"VERDICT: sound with fixes\\n1. [minor] nit\",\"total_cost_usd\":0.05,\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}'\n",
    );
    let out = run(&ok);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("VERDICT: sound with fixes"), "{}", stdout);
    let ledger = std::fs::read_to_string(root.join(".iter/.engine/spend.jsonl")).unwrap();
    assert!(ledger.contains("\"workid\":\"critreview\""), "critic spend is receipted: {}", ledger);
    assert!(take_flag().is_none(), "a delivered review must not flag failure");

    // Exit 3: usage limit — abort immediately, no probe, caller told to STOP.
    let limit = write_stub(
        "critic-limit.sh",
        "#!/bin/sh\necho 'Claude AI usage limit reached|1765500000'\nexit 1\n",
    );
    let out = run(&limit);
    assert_eq!(out.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CRITREVIEW ABORT") && stdout.contains("STOP NOW"), "{}", stdout);
    let flag = take_flag().expect("limit abort writes the fail-flag");
    assert!(flag.contains("usage limit reached"), "raw limit text routes the engine's hold: {}", flag);

    // Exit 1: critic crashes, the haiku probe succeeds (tokens fine), retries burn
    // out — the item is flagged to fail; the caller must NOT proceed unreviewed.
    let crash = write_stub(
        "critic-crash.sh",
        "#!/bin/sh\ncase \"$*\" in\n  *haiku*) echo '{\"type\":\"result\",\"session_id\":\"p1\",\"result\":\"ok\"}';;\n  *) echo 'stub crash' >&2; exit 1;;\nesac\n",
    );
    let out = run(&crash);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("CRITREVIEW FAILED after 2 attempt(s)") && stdout.contains("STOP NOW"), "{}", stdout);
    assert!(stderr.contains("token probe OK"), "probe ran and passed: {}", stderr);
    let flag = take_flag().expect("exhausted retries write the fail-flag");
    assert!(flag.contains("critical review failed after 2 attempt(s)"), "{}", flag);

    // Exit 3: crash where the probe ALSO fails — treated as token exhaustion.
    let dead = write_stub("critic-dead.sh", "#!/bin/sh\necho 'stub crash' >&2\nexit 1\n");
    let out = run(&dead);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stdout).contains("token probe failed"));
    assert!(take_flag().expect("probe-fail abort writes the fail-flag").contains("token probe error"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn critfail_flag_fails_item_even_when_agent_reports_success() {
    // The stub plays an agent whose critreview subprocess wrote the fail-flag
    // mid-turn, but which then LIES by returning a normal successful result.
    // The engine must fail the item from the flag alone.
    let (root, _stub) = setup_project("critflag", 1, 10);
    let stub = root.join("flagging-claude.sh");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf 'critical review failed after 2 attempt(s): stub crash' > \"$ITER_PROJECT/.iter/.engine/critfail-$ITER_WORKID.txt\"\necho '{\"type\":\"result\",\"session_id\":\"s1\",\"result\":\"all done, everything is fine\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    seed(&root, &workitem_json("e2e-critflag-1", "code", "flagged", "./src", "do work with review", "", ""));

    run_engine(&root, &stub, Duration::from_secs(60));

    let closed = closed_items(&root);
    assert_eq!(closed.len(), 1, "{:?}", closed);
    assert_eq!(closed[0]["state"], "failed", "flag must override the agent's claimed success: {:?}", closed[0]);
    assert!(
        closed[0]["lasterror"].as_str().unwrap().contains("critical review failed"),
        "flag reason lands in lasterror: {:?}",
        closed[0]["lasterror"]
    );
    assert!(!root.join(".iter/.engine/critfail-e2e-critflag-1.txt").exists(), "flag is consumed");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn reject_flag_moves_item_to_todo_not_failed() {
    // The stub plays an agent that ran `iter reject` mid-turn (flag written) and
    // then returns a normal successful result. The engine must move the item to
    // todo — the human re-evaluation bucket — never complete, never a retry.
    let (root, _stub) = setup_project("reject", 3, 10);
    let stub = root.join("rejecting-claude.sh");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf 'out of scope: this project has no payment surface' > \"$ITER_PROJECT/.iter/.engine/reject-$ITER_WORKID.txt\"\necho '{\"type\":\"result\",\"session_id\":\"s1\",\"result\":\"rejected, see reason\"}'\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    seed(&root, &workitem_json("e2e-reject-1", "code", "invalid ask", "./src", "order food for delivery", "", ""));

    run_engine(&root, &stub, Duration::from_secs(60));

    assert!(closed_items(&root).is_empty(), "a rejected item is never closed");
    let open = open_items(&root);
    assert_eq!(open.len(), 1, "{:?}", open);
    assert_eq!(open[0]["state"], "todo", "rejection lands in the human-review bucket: {:?}", open[0]);
    assert!(
        open[0]["lasterror"].as_str().unwrap().contains("REJECTED by agent: out of scope"),
        "reason recorded: {:?}",
        open[0]["lasterror"]
    );
    assert!(!root.join(".iter/.engine/reject-e2e-reject-1.txt").exists(), "flag is consumed");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn third_plan_for_same_testgroup_is_held_todo_by_nonconvergence_guard() {
    let (root, _stub) = setup_project("noconverge", 3, 20);
    let add_plan = |title: &str| {
        Command::new(BIN)
            .args([
                "add", "--project", root.to_str().unwrap(), "--type", "plan", "--title", title,
                "--mainwork", "close the gap", "--source-testgroup", "Login E2E",
            ])
            .output()
            .expect("add runs")
    };
    assert!(add_plan("plan lap 1").status.success());
    assert!(add_plan("plan lap 2").status.success());
    let third = add_plan("plan lap 3");
    assert!(third.status.success());
    assert!(
        String::from_utf8_lossy(&third.stderr).contains("non-convergence"),
        "guard announces itself: {}",
        String::from_utf8_lossy(&third.stderr)
    );
    let open = open_items(&root);
    let state = |title: &str| open.iter().find(|i| i["title"] == title).unwrap()["state"].as_str().unwrap().to_string();
    assert_eq!(state("plan lap 1"), "queued", "two laps run freely");
    assert_eq!(state("plan lap 2"), "queued");
    assert_eq!(state("plan lap 3"), "todo", "the third lap waits for a human");
    let lap3 = open.iter().find(|i| i["title"] == "plan lap 3").unwrap();
    assert!(lap3["mainwork"].as_str().unwrap().contains("NON-CONVERGENCE"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn invert_priorities_migrates_open_queue_once() {
    let (root, _stub) = setup_project("prioinv", 3, 10);
    seed(&root, &workitem_json("e2e-inv-1", "code", "urgent old-style", "./src", "w", "", ""));
    // Old higher-is-sooner urgency 8 → new-scheme 2; default 5 stays 5.
    let raw = std::fs::read_to_string(root.join(".iter/.engine/workitems.jsonl")).unwrap();
    std::fs::write(
        root.join(".iter/.engine/workitems.jsonl"),
        raw.replace("\"priority\":5", "\"priority\":8"),
    )
    .unwrap();
    seed(&root, &workitem_json("e2e-inv-2", "code", "default", "./src", "w", "", ""));

    let out = Command::new(BIN)
        .args(["invert-priorities", "--project", root.to_str().unwrap()])
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let open = open_items(&root);
    let prio = |id: &str| open.iter().find(|i| i["workid"] == id).unwrap()["priority"].as_i64().unwrap();
    assert_eq!(prio("e2e-inv-1"), 2, "8 → 2 under newP = 10 - P");
    assert_eq!(prio("e2e-inv-2"), 5, "5 is the fixed point");
    let _ = std::fs::remove_dir_all(&root);
}

/// The TDD loop end-to-end, no engine required: `iter runtests` neutral/claim
/// modes (exit codes, run logs, block updates, the critfail fail-flag) and
/// `iter testsweep` (fix-item birth with provenance, dedup, stale auto-close).
#[test]
fn runtests_claims_and_sweep_lifecycle() {
    let (root, _stub) = setup_project("runtests", 3, 200);
    let test_dir = root.join("comp/test");
    std::fs::create_dir_all(&test_dir).unwrap();
    // The marker file declares the C4 object's tests — mandatory for the sweep.
    std::fs::write(
        root.join("comp/comp.marker.iter.md"),
        "---\nname: \"Comp\"\nlevel: component\ndescription: \"e2e\"\ntestgroup: test/testgroup.iter.md\ntest_dir: test\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(test_dir.join("t1.sh"), "echo 'ITER_RESULT pass=0 fail=1 total=1'\nexit 1\n").unwrap();
    std::fs::write(
        test_dir.join("testgroup.iter.md"),
        "# comp tests\n\n<!-- iterapp:testgroups\n{\"label\":\"G\",\"desc\":\"demo\",\"auto_fix\":false,\"lastrun\":\"\",\"result\":\"\",\"counts\":\"\",\"testlist\":[{\"id\":\"t1\",\"name\":\"one\",\"desc\":\"d\",\"shell\":\"t1.sh\"}]}\n-->\n",
    )
    .unwrap();
    let run = |args: &[&str], workid: Option<&str>| {
        let mut cmd = Command::new(BIN);
        cmd.args(args).arg("--project").arg(root.to_str().unwrap());
        if let Some(w) = workid {
            cmd.env("ITER_WORKID", w);
        }
        cmd.output().unwrap()
    };

    // Neutral run on a red group: exit 1, block stamped, run log captured.
    let out = run(&["runtests", "--group", "G"], None);
    assert_eq!(out.status.code(), Some(1), "neutral red exits 1: {}", String::from_utf8_lossy(&out.stdout));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tests 0/1"), "{}", stdout);
    let tg = std::fs::read_to_string(test_dir.join("testgroup.iter.md")).unwrap();
    assert!(tg.contains("\"result\":\"failed\"") && tg.contains("\"counts\":\"0/1\""), "{}", tg);
    assert!(test_dir.join("runs").read_dir().unwrap().next().is_some(), "run log must exist");

    // --broken upheld on a red group (exit 0); --fixed false → critfail + exit 3.
    assert_eq!(run(&["runtests", "--group", "G", "--broken"], Some("w-e2e")).status.code(), Some(0));
    assert!(!root.join(".iter/.engine/critfail-w-e2e.txt").exists(), "upheld claim writes no flag");
    let out = run(&["runtests", "--group", "G", "--fixed"], Some("w-e2e"));
    assert_eq!(out.status.code(), Some(3), "false --fixed claim exits 3");
    let flag = root.join(".iter/.engine/critfail-w-e2e.txt");
    assert!(flag.exists(), "false claim writes the fail-flag");
    assert!(std::fs::read_to_string(&flag).unwrap().contains("--fixed claim failed"));
    std::fs::remove_file(&flag).unwrap();

    // Sweep on the red group: one code fix item, todo (auto_fix false), with provenance.
    let out = run(&["testsweep"], None);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let open = open_items(&root);
    assert_eq!(open.len(), 1, "{:?}", open);
    assert_eq!(open[0]["type"], "code");
    assert_eq!(open[0]["state"], "todo");
    assert_eq!(open[0]["source"], "testsweep");
    assert_eq!(open[0]["source_testgroup"], "G");
    assert_eq!(open[0]["source_tests"][0], "t1");
    assert_eq!(open[0]["codepath_ignore"][0], "test/");
    // Dedup: a second sweep creates nothing.
    run(&["testsweep"], None);
    assert_eq!(open_items(&root).len(), 1, "dedup guard must hold");

    // The defect gets fixed by other means; sweep goes green and auto-closes the stale item.
    std::fs::write(test_dir.join("t1.sh"), "echo 'ITER_RESULT pass=1 fail=0 total=1'\nexit 0\n").unwrap();
    let out = run(&["testsweep"], None);
    assert!(out.status.success());
    assert!(open_items(&root).is_empty(), "stale unstarted item auto-closes");
    let closed = closed_items(&root);
    assert!(closed.iter().any(|c| c["output"].as_str().unwrap_or("").contains("auto-closed by test sweep")), "{:?}", closed);

    // --broken on a green group is a false claim: stale item, flag written, exit 3.
    let out = run(&["runtests", "--group", "G", "--broken"], Some("w-e2e"));
    assert_eq!(out.status.code(), Some(3));
    assert!(std::fs::read_to_string(root.join(".iter/.engine/critfail-w-e2e.txt")).unwrap().contains("stale"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn itersched_fires_and_shell_executor_runs() {
    let (root, stub) = setup_project("sched", 3, 200);
    // A queued exec:"shell" item: the engine runs the commands directly — no agent.
    seed(&root, r#"{"workid":"e2e-shell-1","type":"chore","state":"queued","exec":"shell","title":"shell run","codepath":".","prework":["echo pre-step"],"mainwork":"echo hello-from-shell","postwork":["echo post-step"],"times":{"added":"2026-01-01T00:00:00Z"}}"#);
    // A schedule template overdue on its interval: fires on the first itersched check.
    seed(&root, r#"{"workid":"e2e-sched-1","type":"chore","state":"scheduled","exec":"shell","title":"minutely echo","codepath":".","mainwork":"echo scheduled-run","priority":8,"sched":{"kind":"every","every_min":1},"times":{"added":"2026-01-01T00:00:00Z"}}"#);

    let stdout = run_engine(&root, &stub, Duration::from_secs(60));

    // Only the template stays open — it is never picked, and its clone completed.
    let open = open_items(&root);
    assert_eq!(open.len(), 1, "open: {:?}", open);
    assert_eq!(open[0]["workid"], "e2e-sched-1");
    assert_eq!(open[0]["state"], "scheduled");
    assert!(
        !open[0]["sched"]["last_fired"].as_str().unwrap_or("").is_empty(),
        "last_fired persists on the template: {:?}",
        open[0]
    );

    let closed = closed_items(&root);
    let shell = closed.iter().find(|i| i["workid"] == "e2e-shell-1").expect("shell item closes");
    assert_eq!(shell["state"], "complete");
    let out = shell["output"].as_str().unwrap();
    assert!(out.contains("pre-step") && out.contains("hello-from-shell") && out.contains("post-step"), "{}", out);
    assert!(!out.contains("fake agent output"), "no LLM turn may run for exec:shell: {}", out);

    let clone = closed
        .iter()
        .find(|i| i["source_schedule"] == "e2e-sched-1")
        .expect("the schedule fired exactly one clone");
    assert_eq!(clone["state"], "complete");
    assert_eq!(clone["source"], "scheduler");
    assert_eq!(clone["priority"], 8, "clone inherits the template's priority");
    assert!(clone["output"].as_str().unwrap().contains("scheduled-run"));

    // Audit trail + engine log line.
    let log = std::fs::read_to_string(root.join(".iter/.engine/sched_log.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1, "{}", log);
    assert!(log.contains("e2e-sched-1"));
    assert!(stdout.contains("fired"), "{}", stdout);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn schedule_api_semantics() {
    let dest = std::env::temp_dir().join(format!("iterloop-e2e-schedapi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(dest.join(".iter/.engine")).unwrap();
    std::fs::write(
        dest.join(".iter/.engine/config.json"),
        r#"{"engine":{"tick_interval_sec":1},"globalsettings":{"log_default_path":""}}"#,
    )
    .unwrap();
    let port = 12000 + (std::process::id() % 20000) as u16;
    let mut child = Command::new(BIN)
        .args(["start", "--project", dest.to_str().unwrap(), "--port", &port.to_string()])
        .env("ITER_CLAUDE_BIN", "/usr/bin/false")
        .env("HOME", &dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "server never came up");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Invalid schedule spec is refused.
    let bad = http(port, "POST", "/api/workitems", Some(r#"{"type":"ghost","title":"bad","mainwork":"m","sched":{"kind":"cron"}}"#));
    assert!(bad.contains("400"), "{}", bad);

    // A schedule template (type "ghost" has no agent, so clones stay queued —
    // that keeps the dedup observable below).
    let created = http(
        port,
        "POST",
        "/api/workitems",
        Some(r#"{"type":"ghost","title":"weekly cleanup","mainwork":"clean things","priority":8,"sched":{"kind":"weekly","day":"sun","at":"22:00","tz":"America/Los_Angeles"}}"#),
    );
    assert!(created.contains("201"), "{}", created);
    let id_at = created.find("\"workid\"").unwrap();
    let id: String = created[id_at..].chars().skip_while(|c| *c != ':').skip(1).filter(|c| c.is_ascii_hexdigit() || *c == '-').take(36).collect();

    // "queue" on a scheduled item MEANS clone-and-queue (engine-owned semantics).
    let fired = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"queue"}"#));
    assert!(fired.contains("200") && fired.contains("\"fired\""), "{}", fired);
    // Dedup: the clone is still open, so a second queue refuses.
    let dup = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"queue"}"#));
    assert!(dup.contains("409"), "{}", dup);
    let list = http(port, "GET", "/api/workitems", None);
    assert!(list.contains("source_schedule"), "{}", list);
    assert!(list.contains("\"scheduler\""), "clone source is scheduler: {}", list);

    // pause ↔ schedule round-trip; complete retires the schedule.
    let paused = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"pause"}"#));
    assert!(paused.contains("paused"), "{}", paused);
    let resched = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"schedule"}"#));
    assert!(resched.contains("scheduled"), "{}", resched);
    let done = http(port, "POST", &format!("/api/workitems/{}/action", id), Some(r#"{"action":"complete"}"#));
    assert!(done.contains("complete"), "{}", done);

    // Agents cannot schedule: `iter add` refuses a schedule spec.
    let f = dest.join("sched-item.json");
    std::fs::write(&f, r#"{"type":"code","mainwork":"m","state":"scheduled","sched":{"kind":"every","every_min":5}}"#).unwrap();
    let out = Command::new(BIN)
        .args(["add", "--project", dest.to_str().unwrap(), "--file", f.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot schedule"), "{}", String::from_utf8_lossy(&out.stderr));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dest);
}

/// Work-item dependencies (workitem_dependency.md), against the real engine
/// loop with the fake runner: B (MORE urgent, disjoint codepath, its own agent
/// type — nothing but the gate holds it) must not dispatch until A closes
/// complete. Break the gate and B starts first, turning this red.
#[test]
fn dependency_gate_orders_dispatch() {
    let (root, stub) = setup_project("depgate", 3, 200);
    std::fs::create_dir_all(root.join("depb")).unwrap();
    seed(&root, &workitem_json("dep-a", "code", "dep a", "./src", "do the foundation work", "", ""));
    seed(
        &root,
        r#"{"workid":"dep-b","title":"dep b","type":"plan","state":"queued","source":"user","priority":0,"risk":0,"codepath":"./depb","depends_on":["dep-a"],"context":[],"testfiles":[],"prework":[],"mainwork":"build on the foundation","postwork":[],"output":"","attempts":0,"lasterror":"","times":{"added":"2026-08-11T00:00:00Z","start":"","preworkdone":"","mainworkdone":"","postworkdone":"","closed":""}}"#,
    );

    run_engine(&root, &stub, Duration::from_secs(120));

    assert!(open_items(&root).is_empty(), "queue must drain");
    let closed = closed_items(&root);
    let a = closed.iter().find(|i| i["workid"] == "dep-a").expect("A closed");
    let b = closed.iter().find(|i| i["workid"] == "dep-b").expect("B closed");
    assert_eq!(a["state"], "complete");
    assert_eq!(b["state"], "complete");
    // The gate in one assertion: despite priority 0, B started only after A closed.
    let a_closed = a["times"]["closed"].as_str().unwrap();
    let b_start = b["times"]["start"].as_str().unwrap();
    assert!(
        b_start >= a_closed,
        "B (P0) must wait for A: B started {} but A closed {}",
        b_start,
        a_closed
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A FAILED dependency never releases the dependent: when A exhausts its
/// attempts, B lands in `todo` carrying a note that names A — it never
/// dispatches, and the engine still reaches idle (no silent hang).
#[test]
fn failed_dependency_lands_dependent_in_todo() {
    let (root, stub) = setup_project("depfail", 1, 200); // max_attempts 1 → first failure is terminal
    std::fs::create_dir_all(root.join("depb")).unwrap();
    seed(&root, &workitem_json("dep-a", "code", "doomed foundation", "./src", "FAIL_TRIGGER", "", ""));
    seed(
        &root,
        r#"{"workid":"dep-b","title":"dep b","type":"code","state":"queued","source":"user","priority":0,"risk":0,"codepath":"./depb","depends_on":["dep-a"],"context":[],"testfiles":[],"prework":[],"mainwork":"build on the foundation","postwork":[],"output":"","attempts":0,"lasterror":"","times":{"added":"2026-08-11T00:00:00Z","start":"","preworkdone":"","mainworkdone":"","postworkdone":"","closed":""}}"#,
    );

    run_engine(&root, &stub, Duration::from_secs(120));

    let closed = closed_items(&root);
    let a = closed.iter().find(|i| i["workid"] == "dep-a").expect("A closed");
    assert_eq!(a["state"], "failed");
    assert!(!closed.iter().any(|i| i["workid"] == "dep-b"), "B must never run or close");
    let open = open_items(&root);
    let b = open.iter().find(|i| i["workid"] == "dep-b").expect("B stays open");
    assert_eq!(b["state"], "todo", "failed dependency flips the dependent to todo for human review");
    let note = b["lasterror"].as_str().unwrap();
    assert!(
        note.contains("DEPENDENCY FAILED") && note.contains("dep-a"),
        "note names the failed dependency: {}",
        note
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// `iter add` dependency plumbing: unique suffixes resolve to full workids
/// (last-12 is the convention), ambiguous or unknown suffixes REFUSE with exit
/// 2 rather than guessing, and a cycle is refused with the path named.
#[test]
fn add_resolves_suffixes_and_refuses_cycles() {
    let (root, _stub) = setup_project("depadd", 3, 200);
    seed(&root, &workitem_json("e2e-dep-parent-123456789abc", "code", "parent", "./src", "parent work", "", ""));
    seed(&root, &workitem_json("e2e-dep-other-99999999-9abc", "code", "other", "./src", "other work", "", ""));

    let add = |extra: &[&str]| {
        let mut args = vec!["add", "--project", root.to_str().unwrap(), "--type", "code", "--mainwork", "child work"];
        args.extend_from_slice(extra);
        Command::new(BIN).args(&args).output().unwrap()
    };

    // Unique last-12 suffix resolves and is stored as the full workid.
    let ok = add(&["--depends-on", "123456789abc", "--title", "gated child"]);
    assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
    let open = open_items(&root);
    let child = open.iter().find(|i| i["title"] == "gated child").expect("child added");
    assert_eq!(child["depends_on"][0], "e2e-dep-parent-123456789abc");

    // Ambiguous suffix: both seeds end in "9abc" — refused, exit 2.
    let ambiguous = add(&["--depends-on", "9abc"]);
    assert_eq!(ambiguous.status.code(), Some(2), "{}", String::from_utf8_lossy(&ambiguous.stderr));
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));

    // Unknown suffix: refused, exit 2.
    let unknown = add(&["--depends-on", "no-such-item"]);
    assert_eq!(unknown.status.code(), Some(2), "{}", String::from_utf8_lossy(&unknown.stderr));

    // Cycle: an open item already depends on "cyc-a"; adding cyc-a depending
    // back on it must refuse with the path named.
    seed(
        &root,
        r#"{"workid":"cyc-b-000000000000","title":"cyc b","type":"code","state":"queued","source":"user","priority":5,"risk":0,"codepath":"./src","depends_on":["cyc-a-111111111111"],"context":[],"testfiles":[],"prework":[],"mainwork":"b","postwork":[],"output":"","attempts":0,"lasterror":"","times":{"added":"2026-08-11T00:00:00Z","start":"","preworkdone":"","mainworkdone":"","postworkdone":"","closed":""}}"#,
    );
    let f = root.join("cycle-item.json");
    std::fs::write(&f, r#"{"workid":"cyc-a-111111111111","type":"code","mainwork":"a","depends_on":["cyc-b-000000000000"]}"#).unwrap();
    let cycle = Command::new(BIN)
        .args(["add", "--project", root.to_str().unwrap(), "--file", f.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(cycle.status.code(), Some(2), "{}", String::from_utf8_lossy(&cycle.stderr));
    let err = String::from_utf8_lossy(&cycle.stderr);
    assert!(err.contains("cycle") && err.contains("111111111111") && err.contains("000000000000"), "{}", err);
    let _ = std::fs::remove_dir_all(&root);
}

/// Stopping an in-progress ("errantly started") item (workitem_stop.md): the
/// stop flag kills the running session mid-turn, the item lands in `todo` with
/// the STOPPED note and its partial state, and `git_start_commit` records the
/// pre-run HEAD — the undo point the webapp's confirmation offers. The stub
/// sleeps 60s, so if the kill doesn't work the engine run times out red.
#[test]
fn stop_in_progress_item_lands_in_todo_with_git_undo_point() {
    let (root, stub) = setup_project("stopitem", 3, 200);

    // Make the project a git repo with one commit — "a git commit prior to starting".
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(root.to_str().unwrap())
            .args(args)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "baseline"]);
    let head = git(&["rev-parse", "HEAD"]);

    // Teach the stub a slow turn, so there is a mid-stream to stop.
    let stub_text = std::fs::read_to_string(&stub).unwrap().replace(
        "case \"$args\" in\n  *FAIL_TRIGGER*)",
        "case \"$args\" in\n  *SLOW_TRIGGER*) sleep 60;;\nesac\ncase \"$args\" in\n  *FAIL_TRIGGER*)",
    );
    assert!(stub_text.contains("SLOW_TRIGGER"), "stub rewrite must apply");
    std::fs::write(&stub, stub_text).unwrap();

    seed(&root, &workitem_json("stop-me", "code", "errantly started", "./src", "SLOW_TRIGGER", "", ""));

    let mut child = Command::new(BIN)
        .args(["run", "--project", root.to_str().unwrap(), "--until-idle"])
        .env("ITER_CLAUDE_BIN", &stub)
        .env("HOME", &root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("engine spawns");

    // Wait for the item to be picked up, then deliver the stop.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "item never reached in-progress");
        if open_items(&root).iter().any(|i| i["workid"] == "stop-me" && i["state"] == "in-progress") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::write(root.join(".iter/.engine/stopitem-stop-me.signal"), "stop requested\n").unwrap();

    // The kill acts mid-turn: the engine must reach idle long before the 60s
    // sleep would end. Break the kill and this times out red.
    let exit_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(status.success(), "engine must exit cleanly after the stop");
                break;
            }
            None => {
                if Instant::now() > exit_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("engine did not reach idle after the stop — mid-turn kill broken?");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    assert!(closed_items(&root).is_empty(), "a stopped item must not close");
    let open = open_items(&root);
    let item = open.iter().find(|i| i["workid"] == "stop-me").expect("item stays open");
    assert_eq!(item["state"], "todo", "stopped work returns to todo for human review");
    let note = item["lasterror"].as_str().unwrap();
    assert!(note.contains("STOPPED by user"), "note explains the stop: {}", note);
    assert_eq!(
        item["git_start_commit"].as_str().unwrap(),
        head,
        "the pre-run HEAD is the recorded undo point"
    );
    assert!(
        !root.join(".iter/.engine/stopitem-stop-me.signal").exists(),
        "the stop flag is consumed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The Test-Loop gate end to end (test_loop_flag.md): `iter testloop --omit`
/// parks a container (flag lands in the marker file; the sweep skips and says
/// so), `--include` on a child re-enters it, and the blocked contract refuses
/// with exit 2 — the agent-facing guard proving it can fail.
#[test]
fn testloop_cli_flags_sweep_and_blocked_refusal() {
    let (root, _stub) = setup_project("testloop", 3, 200);
    let write_marker = |rel: &str, name: &str, level: &str, extra: &str| {
        let dir = root.join(rel);
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(
            dir.join(format!("{}.marker.iter.md", name)),
            format!("---\nname: \"{}\"\nlevel: {}\ntestgroup: test/testgroup.iter.md\ntest_dir: test\n{}---\nbody\n", name, level, extra),
        )
        .unwrap();
        std::fs::write(
            dir.join("test/testgroup.iter.md"),
            "# tests\n```iterapp:testgroups\n{\"label\":\"G-".to_string() + name + "\",\"desc\":\"d\",\"auto_fix\":false,\"testlist\":[]}\n```\n",
        )
        .unwrap();
    };
    write_marker("ctr", "ctr", "container", "");
    write_marker("ctr/api", "api", "component", "");
    write_marker("vend", "vend", "container", "test_loop: blocked\n");

    let run = |args: &[&str]| {
        let mut a = vec!["testloop", "--project", root.to_str().unwrap()];
        a.extend_from_slice(args);
        Command::new(BIN).args(&a).output().unwrap()
    };

    // Omit the container: the flag lands in the marker file.
    let out = run(&["--omit", "ctr"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(std::fs::read_to_string(root.join("ctr/ctr.marker.iter.md")).unwrap().contains("test_loop: omit"));

    // The sweep skips the omitted subtree and reports it — no silent holes.
    let sweep = Command::new(BIN)
        .args(["testsweep", "--project", root.to_str().unwrap()])
        .env("HOME", &root)
        .output()
        .unwrap();
    let sweep_out = String::from_utf8_lossy(&sweep.stdout);
    assert!(sweep_out.contains("3 omitted by test_loop flag"), "ctr + api (carry-down) + vend: {}", sweep_out);

    // Include the child: re-entered under the still-omitted parent.
    let out = run(&["--include", "api"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let list = run(&["--list"]);
    let text = String::from_utf8_lossy(&list.stdout).into_owned();
    assert!(text.contains("included") && text.contains("OMITTED"), "{}", text);

    // Blocked contract: include refuses (exit 2), the flag survives.
    let out = run(&["--include", "vend"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("blocked"));
    assert!(std::fs::read_to_string(root.join("vend/vend.marker.iter.md")).unwrap().contains("test_loop: blocked"));
    let _ = std::fs::remove_dir_all(&root);
}
