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
    // A marker tree: node, interface, use-case, plain doc.
    std::fs::create_dir_all(dest.join("svc")).unwrap();
    std::fs::write(dest.join("root.iter.md"), "---\nname: API Test Project\nlevel: project\n---\n").unwrap();
    std::fs::write(
        dest.join("svc/svc.iter.md"),
        "---\nname: Svc\nlevel: component\nuses: [pay-api]\nprovides: [svc-api]\n---\ncontext",
    )
    .unwrap();
    std::fs::write(
        dest.join("svc/svc-api.iter.md"),
        "---\ninterface: svc-api\nkind: http\nendpoint: GET /svc\n---\ncontract",
    )
    .unwrap();
    std::fs::write(dest.join("svc/bizreq.iter.md"), "plain context\n").unwrap();

    let port = 22000 + (std::process::id() % 20000) as u16;
    let mut child = Command::new(BIN)
        .args(["start", "--project", dest.to_str().unwrap(), "--port", &port.to_string()])
        .env("ITER_CLAUDE_BIN", "/usr/bin/false") // any picked-up item fails fast, no real claude
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
