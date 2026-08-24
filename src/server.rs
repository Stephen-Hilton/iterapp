use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::config;
use crate::context;
use crate::locks;
use crate::markers;
use crate::registry;
use crate::scheduler;
use crate::testgroups;
use crate::workitems::{self, Queue, WorkItem};

/// The webapp page, embedded so the deployed binary stays one file.
const PAGE: &str = include_str!("webapp/app.html");

/* ------------------------------------------------------------ engine control */

/// The engine loop as a restartable thread, so the webapp's pause/resume works:
/// `iter stop` (or POST /api/engine {"action":"stop"}) ends the loop but the server
/// stays up; resume clears the signal and spawns a fresh loop.
pub struct Engine {
    pub project: PathBuf,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Engine {
    pub fn new(project: PathBuf) -> Arc<Engine> {
        Arc::new(Engine { project, handle: Mutex::new(None) })
    }

    pub fn start_loop(&self) -> bool {
        let mut guard = self.handle.lock().unwrap();
        if guard.as_ref().map(|h| !h.is_finished()).unwrap_or(false) {
            return false; // already running
        }
        let project = self.project.clone();
        *guard = Some(std::thread::spawn(move || {
            if let Err(e) = scheduler::run(project, scheduler::RunMode { once: false, until_idle: false }) {
                crate::logging::error("engine", &e);
            }
        }));
        true
    }

    pub fn state(&self) -> &'static str {
        let running = self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false);
        let signal = scheduler::stop_signal_path(&self.project);
        match (running, signal.exists()) {
            (true, false) => "running",
            (true, true) => {
                if std::fs::read_to_string(&signal).map(|t| t.contains("drain")).unwrap_or(false) {
                    "draining"
                } else {
                    "stopping"
                }
            }
            (false, _) => "stopped",
        }
    }
}

/* ------------------------------------------------------------ bind + slug */

/// Deterministic auto-port: hash the project's absolute path into 9700–9899 so the
/// same project gets the same port every restart, probing upward on a clash.
pub fn bind(project_root: &Path, want: Option<u16>) -> std::io::Result<(TcpListener, u16)> {
    if let Some(port) = want {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        return Ok((listener, port));
    }
    let canon = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let mut hash: u32 = 0;
    for byte in canon.to_string_lossy().bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
    }
    for offset in 0..200u32 {
        let port = 9700 + (((hash % 200) + offset) % 200) as u16;
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok((listener, port));
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

pub fn slug(project_root: &Path) -> String {
    crate::project::Project::load(project_root).slug()
}

/// The structureV2 project head as one JSON view: the server config
/// (.iter/config.iter.json), the main.iter.md frontmatter, and the resolved
/// paths — what the Settings page renders and PUTs back.
pub fn project_settings(project_root: &Path) -> Value {
    let p = crate::project::Project::load(project_root);
    json!({
        "server": p.server,
        "project": {
            "projectname": p.config.projectname,
            "projectdescription": p.config.projectdescription,
            "globalscandirs": p.config.globalscandirs,
            "globalinterfacedir": p.config.globalinterfacedir,
            "globalusecasedir": p.config.globalusecasedir,
            "globalcontextfiles": p.config.globalcontextfiles,
        },
        "resolved": {
            "topdir": p.topdir.to_string_lossy(),
            "mainfile": p.mainfile.to_string_lossy(),
            "interfacedir": p.interfacedir.to_string_lossy(),
            "usecasedir": p.usecasedir.to_string_lossy(),
            "scandirs": p.scandirs.iter().map(|d| d.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        },
        // Legacy aliases a few readers still use.
        "project_name": p.projectname(),
        "url_slug": p.slug(),
        "default_context": p.server.default_context,
    })
}

/// The project's scan roots — structureV2's resolved `globalscandirs`.
pub fn scan_roots(project: &Path) -> Vec<PathBuf> {
    crate::project::Project::load(project).scandirs
}

/* ------------------------------------------------------------ http plumbing */

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Vec<u8>,
}

struct Resp {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn json_resp(status: u16, value: Value) -> Resp {
    Resp { status, content_type: "application/json", body: value.to_string().into_bytes() }
}

fn err_resp(status: u16, msg: &str) -> Resp {
    json_resp(status, json!({ "error": msg }))
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        _ => "Internal Server Error",
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 1_048_576 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_uppercase();
    let target = parts.next()?;
    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(4_194_304);
    let mut body: Vec<u8> = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    let (path, query_str) = target.split_once('?').unwrap_or((target, ""));
    let mut query = HashMap::new();
    for pair in query_str.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(k.to_string(), v.replace("%20", " ").replace('+', " "));
    }
    Some(Req { method, path: path.trim_end_matches('/').to_string(), query, body })
}

fn write_resp(stream: &mut TcpStream, resp: Resp) {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        resp.body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
}

/* ------------------------------------------------------------ serve loop */

pub fn serve(listener: TcpListener, engine: Arc<Engine>, port: u16) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let engine = Arc::clone(&engine);
            std::thread::spawn(move || {
                let Some(req) = read_request(&mut stream) else { return };
                if req.path == "/api/events" {
                    sse_events(stream, &engine.project);
                    return;
                }
                let resp = route(&req, &engine, port);
                write_resp(&mut stream, resp);
            });
        }
    });
}

fn route(req: &Req, engine: &Engine, port: u16) -> Resp {
    let project = &engine.project;
    let segments: Vec<&str> = req.path.trim_start_matches('/').split('/').collect();
    match (req.method.as_str(), segments.as_slice()) {
        (_, [""]) | ("GET", _) if !req.path.starts_with("/api") => {
            Resp { status: 200, content_type: "text/html", body: PAGE.as_bytes().to_vec() }
        }
        ("GET", ["api", "state"]) => api_state(project, engine, port),
        ("POST", ["api", "engine"]) => api_engine(req, engine),
        ("GET", ["api", "meta"]) => api_meta(project),
        ("GET", ["api", "workitems"]) => api_list(project),
        ("POST", ["api", "workitems"]) => api_create(req, project),
        ("GET", ["api", "workitems", id]) => api_get(project, id),
        ("PATCH", ["api", "workitems", id]) => api_patch(req, project, id),
        ("POST", ["api", "workitems", id, "action"]) => api_action(req, project, id),
        ("GET", ["api", "workitems", id, "logs"]) => api_logs(project, id),
        ("GET", ["api", "workitems", id, "tests"]) => api_tests(project, id),
        ("GET", ["api", "history"]) => api_history(req, project),
        ("GET", ["api", "config"]) => api_config_get(project),
        ("PUT", ["api", "config"]) => api_config_put(req, project),
        ("GET", ["api", "agents"]) => api_agents_get(project),
        ("PUT", ["api", "agents", name]) => api_agent_put(req, project, name),
        ("GET", ["api", "projectsettings"]) => json_resp(200, project_settings(project)),
        ("PUT", ["api", "projectsettings"]) => api_projectsettings_put(req, project),
        ("GET", ["api", "markers"]) | ("POST", ["api", "markers", "rescan"]) => api_markers(project),
        ("POST", ["api", "orphans", "link"]) => api_orphan_link(req, project),
        ("POST", ["api", "file", "read"]) => api_file_read(req, project),
        ("PUT", ["api", "file"]) => api_file_write(req, project),
        ("GET", ["api", "testgroups"]) => api_testgroups(project),
        ("POST", ["api", "testgroups", "autofix"]) => api_testgroups_autofix(req, project),
        ("POST", ["api", "teststate"]) | ("POST", ["api", "testloop"]) => api_teststate(req, project),
        ("POST", ["api", "testruns"]) => api_testruns(req, project),
        ("POST", ["api", "validate"]) => api_validate(req, project),
        ("POST", ["api", "usecases"]) => api_usecases(req, project, "create"),
        ("PUT", ["api", "usecases"]) => api_usecases(req, project, "update"),
        ("POST", ["api", "usecases", "delete"]) => api_usecases(req, project, "delete"),
        ("GET", ["api", "servers"]) => {
            json_resp(200, serde_json::to_value(registry::live()).unwrap_or_else(|_| json!([])))
        }
        ("GET", _) => err_resp(404, "no such endpoint"),
        _ => err_resp(405, "method not allowed"),
    }
}

/* ------------------------------------------------------------ handlers */

fn queue_for(project: &Path) -> Queue {
    let cfg = config::load(project);
    Queue::new(project, &cfg)
}

fn api_state(project: &Path, engine: &Engine, port: u16) -> Resp {
    let queue = queue_for(project);
    let open = queue.load();
    let closed = queue.load_closed();
    let count = |s: &str| open.iter().filter(|i| i.state == s).count();
    let cfg = config::load(project);
    json_resp(
        200,
        json!({
            "engine": engine.state(),
            "port": port,
            "project": project.to_string_lossy(),
            "slug": slug(project),
            "spend_today_usd": crate::spend::today_usd(project),
            "budget_usd_per_day": cfg.engine.max_cost_usd_per_day,
            "usage": crate::limits::read_snapshot(&cfg).map(|u| {
                let now = Utc::now();
                json!({
                    "five_hour_pct": u.five_hour_pct,
                    "seven_day_pct": u.seven_day_pct,
                    "effective_pct": u.effective_pct(now),
                    "age_sec": u.age_sec(now),
                })
            }).unwrap_or(Value::Null),
            "counts": {
                "queued": count(workitems::STATE_QUEUED),
                "in-progress": count(workitems::STATE_IN_PROGRESS),
                "todo": count(workitems::STATE_TODO),
                "paused": count(workitems::STATE_PAUSED),
                "failed": count(workitems::STATE_FAILED)
                    + closed.iter().filter(|i| i.state == workitems::STATE_FAILED).count(),
                "complete": closed.iter().filter(|i| i.state == workitems::STATE_COMPLETE).count(),
                "open": open.len(),
                "total": open.len() + closed.len(),
            }
        }),
    )
}

fn api_engine(req: &Req, engine: &Engine) -> Resp {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let action = body.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let signal = scheduler::stop_signal_path(&engine.project);
    match action {
        "pause" => {
            let _ = std::fs::write(&signal, format!("{} drain requested via webapp\n", workitems::now_iso()));
            json_resp(200, json!({ "engine": engine.state() }))
        }
        "stop" => {
            let _ = std::fs::write(&signal, format!("{} requested via webapp\n", workitems::now_iso()));
            json_resp(200, json!({ "engine": engine.state() }))
        }
        "resume" => {
            let _ = std::fs::remove_file(&signal);
            engine.start_loop();
            json_resp(200, json!({ "engine": engine.state() }))
        }
        "shutdown" => {
            registry::deregister();
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(0);
            });
            json_resp(200, json!({ "engine": "shutdown" }))
        }
        _ => err_resp(400, "action must be pause|stop|resume|shutdown"),
    }
}

fn api_meta(project: &Path) -> Resp {
    let agents: Vec<Value> = crate::agents::discover(project)
        .into_iter()
        .map(|a| {
            json!({ "type": a.type_name, "description": a.description, "visible": a.visible,
                    "max_agent_count": a.max_agent_count, "model": a.model })
        })
        .collect();
    // .md files are prompt steps (run in the agent conversation); .sh files are
    // shell steps — flagged so the UI can render them differently.
    let mut prepost: Vec<Value> = std::fs::read_dir(project.join(".iter/prepostwork"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let ext = p.extension().and_then(|x| x.to_str())?;
                    let exec = match ext {
                        "md" => "agent",
                        "sh" => "shell",
                        _ => return None,
                    };
                    let name = p.file_stem()?.to_string_lossy().into_owned();
                    Some(json!({ "name": name, "exec": exec }))
                })
                .collect()
        })
        .unwrap_or_default();
    prepost.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let p = crate::project::Project::load(project);
    json_resp(
        200,
        json!({
            "agents": agents,
            "prepostwork": prepost,
            "project_root": project.canonicalize().unwrap_or_else(|_| project.to_path_buf()).to_string_lossy(),
            "code_root": p.topdir.to_string_lossy(),
            "mainfile": p.mainfile.to_string_lossy(),
            "context_files": p.context_files().iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "project_name": p.projectname(),
            "url_slug": p.slug(),
            "default_context": p.server.default_context,
            "home": std::env::var("HOME").unwrap_or_default(),
        }),
    )
}

fn api_list(project: &Path) -> Resp {
    let queue = queue_for(project);
    let open = queue.load();
    let closed = queue.load_closed();
    // Dependency-gated items get their gate's live status injected (computed
    // against the FULL closed archive, before truncation), so the webapp header
    // can show the blocked-by chain without re-deriving engine semantics.
    let open_vals: Vec<Value> = open
        .iter()
        .map(|i| {
            let mut v = serde_json::to_value(i).expect("workitem serializes");
            if !i.depends_on.is_empty() {
                let (state, by) = match workitems::dep_status(i, &open, &closed) {
                    workitems::DepStatus::Satisfied => ("ok", Value::Null),
                    workitems::DepStatus::Blocked(id) => ("blocked", json!(id)),
                    workitems::DepStatus::Failed(why) => ("failed", json!(why)),
                };
                v["dep_status"] = json!({ "state": state, "by": by });
            }
            v
        })
        .collect();
    let mut closed = closed;
    if closed.len() > 500 {
        closed = closed.split_off(closed.len() - 500);
    }
    json_resp(200, json!({ "open": open_vals, "closed": closed }))
}

fn api_create(req: &Req, project: &Path) -> Resp {
    let Ok(mut item) = serde_json::from_slice::<WorkItem>(&req.body) else {
        return err_resp(400, "body must be a work item object");
    };
    if item.item_type.is_empty() || item.mainwork.is_empty() {
        return err_resp(400, "a work item needs at least type and mainwork");
    }
    let cfg = config::load(project);
    let queue = queue_for(project);
    if queue.load().len() >= cfg.engine.max_open_workitems {
        return err_resp(409, &format!("queue at max_open_workitems ({})", cfg.engine.max_open_workitems));
    }
    if item.workid.is_empty() {
        item.workid = uuid::Uuid::new_v4().to_string();
    }
    if item.times.added.is_empty() {
        item.times.added = workitems::now_iso();
    }
    if !item.sched.is_none() {
        // A schedule template (itersched.rs): user-created only — this API is the
        // user's path; `iter add` (the agents' path) refuses schedules. Valid
        // states are scheduled (live) or paused (schedule off).
        if !matches!(item.sched.kind.as_str(), "every" | "daily" | "weekly" | "stale") {
            return err_resp(400, "sched.kind must be every|daily|weekly|stale");
        }
        if !matches!(item.state.as_str(), "scheduled" | "paused") {
            item.state = workitems::STATE_SCHEDULED.into();
        }
    } else if !matches!(item.state.as_str(), "queued" | "todo" | "paused") {
        item.state = workitems::STATE_QUEUED.into();
    }
    {
        let mode = item.automation.trim();
        if !mode.is_empty() && mode != workitems::AUTOMATION_REVIEW && mode != workitems::AUTOMATION_AUTO {
            return err_resp(400, &format!("automation must be \"review\" or \"auto\" (got \"{}\")", mode));
        }
    }
    if !item.depends_on.is_empty() {
        // Schedules are cadence-driven; mixing gates and cadence invites a
        // silent never-runs, so templates never carry dependencies.
        if !item.sched.is_none() {
            return err_resp(400, "schedule templates cannot have depends_on — schedules are cadence-driven, not gated");
        }
        let open = queue.load();
        let closed = queue.load_closed();
        if let Err(e) = workitems::resolve_depends_on(&mut item, &open, &closed) {
            return err_resp(400, &e);
        }
    }
    let known: Vec<String> = crate::agents::discover(project).into_iter().map(|a| a.type_name).collect();
    let warning = (item.exec != workitems::EXEC_SHELL && !known.is_empty() && !known.contains(&item.item_type))
        .then(|| format!("type \"{}\" matches no agent in .iter/agents/", item.item_type));
    if let Err(e) = queue.append(&item) {
        return err_resp(500, &format!("cannot append: {}", e));
    }
    json_resp(201, json!({ "workid": item.workid, "warning": warning }))
}

fn find_item(project: &Path, id: &str) -> Option<(WorkItem, bool)> {
    let queue = queue_for(project);
    if let Some(i) = queue.load().into_iter().find(|i| i.workid == id) {
        return Some((i, true));
    }
    queue.load_closed().into_iter().rev().find(|i| i.workid == id).map(|i| (i, false))
}

fn api_get(project: &Path, id: &str) -> Resp {
    match find_item(project, id) {
        Some((item, open)) => json_resp(200, json!({ "item": item, "open": open })),
        None => err_resp(404, "no such work item"),
    }
}

const EDITABLE: &[&str] = &[
    "title", "type", "priority", "risk", "source", "codepath", "codepath_ignore", "context", "testfiles", "prework",
    "postwork", "mainwork", "exec", "sched", "depends_on", "depends_on_shallow", "automation",
];

fn api_patch(req: &Req, project: &Path, id: &str) -> Resp {
    let Ok(Value::Object(patch)) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object of editable fields");
    };
    let queue = queue_for(project);
    let closed = queue.load_closed();
    let result = queue.with_lock(|items| {
        let Some(pos) = items.iter().position(|i| i.workid == id) else {
            return Err((404, "no such open work item".to_string()));
        };
        if items[pos].state != workitems::STATE_TODO
            && items[pos].state != workitems::STATE_PAUSED
            && items[pos].state != workitems::STATE_SCHEDULED
        {
            return Err((409, format!("only todo/paused/scheduled items are editable (state: {})", items[pos].state)));
        }
        let mut v = serde_json::to_value(&items[pos]).expect("serializes");
        for (key, val) in &patch {
            if EDITABLE.contains(&key.as_str()) {
                v[key] = val.clone();
            }
        }
        match serde_json::from_value::<WorkItem>(v) {
            Ok(mut updated) => {
                let mode = updated.automation.trim();
                if !mode.is_empty() && mode != workitems::AUTOMATION_REVIEW && mode != workitems::AUTOMATION_AUTO {
                    return Err((400, format!("automation must be \"review\" or \"auto\" (got \"{}\")", mode)));
                }
                if !updated.depends_on.is_empty() {
                    if !updated.sched.is_none() {
                        return Err((400, "schedule templates cannot have depends_on — schedules are cadence-driven, not gated".to_string()));
                    }
                    // Same refusal rules as create: unknown/ambiguous suffixes
                    // and cycles never land in the queue.
                    if let Err(e) = workitems::resolve_depends_on(&mut updated, items, &closed) {
                        return Err((400, e));
                    }
                }
                items[pos] = updated;
                Ok(items[pos].clone())
            }
            Err(e) => Err((400, format!("invalid patch: {}", e))),
        }
    });
    match result {
        Ok(Ok(item)) => json_resp(200, json!({ "item": item })),
        Ok(Err((code, msg))) => err_resp(code, &msg),
        Err(e) => err_resp(500, &e.to_string()),
    }
}

fn api_action(req: &Req, project: &Path, id: &str) -> Resp {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let action = body.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let queue = queue_for(project);

    // Stop an IN-PROGRESS ("errantly started") item: the engine owns it, so
    // the stop is delivered as a file flag — the runner polls it mid-turn and
    // kills the session; the worker moves the item to `todo` with partial
    // output kept. The webapp confirms with the user before calling this
    // (mid-stream stop, partially completed work, git undo hint).
    if action == "stop" {
        let Some(item) = queue.load().into_iter().find(|i| i.workid == id) else {
            return err_resp(404, "no such open work item");
        };
        if item.state != workitems::STATE_IN_PROGRESS {
            return err_resp(409, &format!("only in-progress items can be stopped (state: {})", item.state));
        }
        let flag = crate::scheduler::stopitem_path(project, &item.workid);
        return match std::fs::write(&flag, format!("{} stop requested via webapp\n", workitems::now_iso())) {
            Ok(()) => json_resp(200, json!({ "state": "stopping", "git_start_commit": item.git_start_commit })),
            Err(e) => err_resp(500, &format!("cannot write stop flag: {}", e)),
        };
    }

    // Engine-owned semantics (not a UI convention): "queueing" a SCHEDULED item
    // means clone-and-queue a run of it — the template itself never queues.
    // itersched::fire dedups under the record lock, so this happens before (not
    // inside) the with_lock below.
    if matches!(action, "queue" | "requeue")
        && queue.load().iter().any(|i| i.workid == id && i.state == workitems::STATE_SCHEDULED)
    {
        let cfg = config::load(project);
        return match crate::itersched::fire(project, &cfg, id, "manual queue") {
            Ok(Some(clone)) => json_resp(200, json!({ "state": "scheduled", "workid": clone, "fired": true })),
            Ok(None) => err_resp(409, "an earlier run of this schedule is still open — no duplicate created"),
            Err(e) => err_resp(500, &e),
        };
    }

    let result = queue.with_lock(|items| {
        let Some(pos) = items.iter().position(|i| i.workid == id) else {
            return Err((404, "no such open work item".to_string()));
        };
        if items[pos].state == workitems::STATE_IN_PROGRESS && action != "clone" {
            return Err((409, "item is in-progress; the engine owns it until it finishes".to_string()));
        }
        match action {
            "queue" | "requeue" => {
                items[pos].state = workitems::STATE_QUEUED.into();
                items[pos].lasterror.clear();
                Ok(json!({ "state": "queued" }))
            }
            "todo" => {
                items[pos].state = workitems::STATE_TODO.into();
                Ok(json!({ "state": "todo" }))
            }
            "pause" => {
                items[pos].state = workitems::STATE_PAUSED.into();
                Ok(json!({ "state": "paused" }))
            }
            "schedule" => {
                // todo/paused → scheduled (needs a schedule spec on the item).
                if items[pos].sched.is_none() {
                    return Err((400, "item has no sched spec — set sched.kind (every|daily|weekly|stale) first".to_string()));
                }
                items[pos].state = workitems::STATE_SCHEDULED.into();
                Ok(json!({ "state": "scheduled" }))
            }
            "complete" => {
                items[pos].state = workitems::STATE_COMPLETE.into();
                items[pos].times.closed = workitems::now_iso();
                let done = items.remove(pos);
                Ok(json!({ "state": "complete", "closed_item": done }))
            }
            "delete" => {
                items.remove(pos);
                Ok(json!({ "state": "deleted" }))
            }
            "clone" => {
                let mut copy = items[pos].clone();
                copy.workid = uuid::Uuid::new_v4().to_string();
                copy.state = workitems::STATE_TODO.into();
                copy.attempts = 0;
                copy.output.clear();
                copy.lasterror.clear();
                copy.times = workitems::Times { added: workitems::now_iso(), ..Default::default() };
                let workid = copy.workid.clone();
                items.push(copy);
                Ok(json!({ "state": "todo", "workid": workid }))
            }
            _ => Err((400, "action must be queue|todo|pause|schedule|complete|delete|clone|stop".to_string())),
        }
    });
    match result {
        Ok(Ok(v)) => {
            // complete → archive outside the open-queue lock
            if let Some(done) = v.get("closed_item") {
                if let Ok(item) = serde_json::from_value::<WorkItem>(done.clone()) {
                    let _ = queue.append_closed(&item);
                }
                return json_resp(200, json!({ "state": "complete" }));
            }
            json_resp(200, v)
        }
        Ok(Err((code, msg))) => err_resp(code, &msg),
        Err(e) => err_resp(500, &e.to_string()),
    }
}

fn api_logs(project: &Path, id: &str) -> Resp {
    let short: String = id.chars().take(8).collect();
    let cfg = config::load(project);
    let mut lines: Vec<String> = Vec::new();
    if !cfg.globalsettings.log_default_path.is_empty() {
        let log_dir = project.join(
            Path::new(&cfg.globalsettings.log_default_path).parent().unwrap_or(Path::new("logs")),
        );
        let mut files: Vec<PathBuf> = std::fs::read_dir(&log_dir)
            .map(|e| e.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
            .unwrap_or_default();
        files.sort();
        let mut tag = String::new();
        for file in files.iter().rev().take(2).rev() {
            let Ok(text) = std::fs::read_to_string(file) else { continue };
            for line in text.lines() {
                if line.contains(&short) {
                    if let (Some(open_b), Some(close_b)) = (line.find('['), line.find(']')) {
                        tag = line[open_b..=close_b].to_string();
                    }
                    lines.push(line.to_string());
                } else if !tag.is_empty() && line.contains(&tag) {
                    lines.push(line.to_string());
                }
            }
        }
    }
    if lines.len() > 800 {
        lines = lines.split_off(lines.len() - 800);
    }
    json_resp(200, json!({ "lines": lines }))
}

fn api_tests(project: &Path, id: &str) -> Resp {
    let Some((item, _)) = find_item(project, id) else { return err_resp(404, "no such work item") };
    let cfg = config::load(project);
    let code_root = config::code_root(project, &cfg);
    let codepath = if Path::new(&item.codepath).is_absolute() {
        PathBuf::from(&item.codepath)
    } else {
        code_root.join(&item.codepath)
    };
    let (files, _warnings) = context::resolve(&item.testfiles, &codepath, &code_root);
    let groups: Vec<Value> = files
        .iter()
        .filter_map(|f| {
            let text = std::fs::read_to_string(f).ok()?;
            Some(json!({ "file": f.to_string_lossy(), "groups": testgroups::parse(&text) }))
        })
        .collect();
    json_resp(200, json!({ "testfiles": groups }))
}

fn api_history(req: &Req, project: &Path) -> Resp {
    let days: i64 = req.query.get("days").and_then(|d| d.parse().ok()).unwrap_or(14);
    let end = req
        .query
        .get("end")
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive());
    let start = req
        .query
        .get("start")
        .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| end - ChronoDuration::days(days.clamp(1, 366) - 1));

    let queue = queue_for(project);
    let closed = queue.load_closed();
    let open = queue.load();

    let mut day_buckets: Vec<Value> = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let prefix = cursor.format("%Y-%m-%d").to_string();
        let complete = closed
            .iter()
            .filter(|i| i.state == workitems::STATE_COMPLETE && i.times.closed.starts_with(&prefix))
            .count();
        let failed = closed
            .iter()
            .filter(|i| i.state == workitems::STATE_FAILED && i.times.closed.starts_with(&prefix))
            .count();
        day_buckets.push(json!({ "date": prefix, "complete": complete, "failed": failed }));
        cursor += ChronoDuration::days(1);
    }

    let in_window = |i: &&WorkItem| {
        workitems::parse_iso(&i.times.closed)
            .map(|t| {
                let d = t.date_naive();
                d >= start && d <= end
            })
            .unwrap_or(false)
    };
    let window: Vec<&WorkItem> = closed.iter().filter(in_window).collect();
    let complete_n = window.iter().filter(|i| i.state == workitems::STATE_COMPLETE).count();
    let mut by_agent: HashMap<String, usize> = HashMap::new();
    for item in window.iter().filter(|i| i.state == workitems::STATE_COMPLETE) {
        *by_agent.entry(item.item_type.clone()).or_default() += 1;
    }
    let mut cycles: Vec<i64> = window
        .iter()
        .filter_map(|i| {
            Some((workitems::parse_iso(&i.times.closed)? - workitems::parse_iso(&i.times.added)?).num_seconds())
        })
        .collect();
    cycles.sort();
    let median_cycle_sec = cycles.get(cycles.len() / 2).copied().unwrap_or(0);
    let hours = ((end - start).num_days() + 1) * 24;
    let handoff = window.iter().filter(|i| i.source.starts_with("agent")).count();

    json_resp(
        200,
        json!({
            "start": start.format("%Y-%m-%d").to_string(),
            "end": end.format("%Y-%m-%d").to_string(),
            "days": day_buckets,
            "by_agent": by_agent,
            "tiles": {
                "spend_today_usd": crate::spend::today_usd(project),
                "budget_usd_per_day": config::load(project).engine.max_cost_usd_per_day,
                "success_pct": if window.is_empty() { 100 } else { (100 * complete_n) / window.len() },
                "per_hour": if hours > 0 { complete_n as f64 / hours as f64 } else { 0.0 },
                "median_cycle_sec": median_cycle_sec,
                "running": open.iter().filter(|i| i.state == workitems::STATE_IN_PROGRESS).count(),
                "open": open.len(),
                "handoff_pct": if window.is_empty() { 0 } else { (100 * handoff) / window.len() },
            }
        }),
    )
}

fn api_config_get(project: &Path) -> Resp {
    let path = config::engine_dir(project).join("config.json");
    match std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        Some(v) => json_resp(200, v),
        None => json_resp(200, serde_json::to_value(config::Config::default()).unwrap_or(Value::Null)),
    }
}

fn api_config_put(req: &Req, project: &Path) -> Resp {
    let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be JSON");
    };
    if serde_json::from_value::<config::Config>(v.clone()).is_err() {
        return err_resp(400, "not a valid iterloop config");
    }
    let path = config::engine_dir(project).join("config.json");
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        return err_resp(500, "cannot write config.json");
    }
    json_resp(200, v)
}

/// Resolve `default_codepath` / `default_codepath_ignore` placeholders
/// ({usecase_dir}/{interface_dir}/{test_dir}) to paths RELATIVE to the code
/// root, so the pre-filled value reads the way a user would type it
/// ("usecases", not an absolute path; "{test_dir}/" → "tests/" on pdy).
fn resolve_default_codepath(project: &Path, raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    let cfg = config::load(project);
    let code_root = config::code_root(project, &cfg);
    let rel = |p: std::path::PathBuf| match p.strip_prefix(&code_root) {
        Ok(r) if !r.as_os_str().is_empty() => r.to_string_lossy().into_owned(),
        Ok(_) => ".".to_string(),
        Err(_) => p.to_string_lossy().into_owned(),
    };
    raw.replace("{usecase_dir}", &rel(config::usecase_dir(project, &cfg)))
        .replace("{interface_dir}", &rel(config::interface_dir(project, &cfg)))
        .replace("{test_dir}", &cfg.globalsettings.test_dir)
}

fn agent_json(project: &Path, a: &crate::agents::AgentDef) -> Value {
    json!({
        "type": a.type_name, "description": a.description, "visible": a.visible,
        "max_agent_count": a.max_agent_count, "max_work_timeout_sec": a.max_work_timeout_sec,
        "max_connection_timeout_sec": a.max_connection_timeout_sec, "model": a.model,
        "model_flags": a.model_flags, "llm_run_mode": a.llm_run_mode,
        "sleep_interval_sec": a.sleep_interval_sec,
        "default_codepath": a.default_codepath,
        "default_codepath_resolved": resolve_default_codepath(project, &a.default_codepath),
        "default_codepath_ignore": a.default_codepath_ignore,
        "default_codepath_ignore_resolved": resolve_default_codepath(project, &a.default_codepath_ignore),
        "body": a.body,
    })
}

fn api_agents_get(project: &Path) -> Resp {
    let agents: Vec<Value> =
        crate::agents::discover(project).iter().map(|a| agent_json(project, a)).collect();
    json_resp(200, json!({ "agents": agents }))
}

/// Update one agent's frontmatter/body in `.iter/agents/<name>.md`. Editing only:
/// a new agent is added by adding a new file, so an unknown name is a 404.
fn api_agent_put(req: &Req, project: &Path, name: &str) -> Resp {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return err_resp(400, "agent name must be alphanumeric with - or _");
    }
    let path = crate::agents::agents_dir(project).join(format!("{}.md", name));
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return err_resp(404, "no such agent — add one by creating .iter/agents/<name>.md");
    };
    let Ok(Value::Object(patch)) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object of agent fields");
    };
    let mut updates: Vec<(String, String)> = Vec::new();
    let mut new_body: Option<String> = None;
    for (key, val) in &patch {
        if key == "body" {
            match val.as_str() {
                Some(b) => new_body = Some(b.to_string()),
                None => return err_resp(400, "body must be a string"),
            }
            continue;
        }
        if !crate::agents::EDITABLE_KEYS.contains(&key.as_str()) {
            return err_resp(400, &format!("unknown agent field \"{}\"", key));
        }
        let text = match key.as_str() {
            "visible" => match val {
                Value::Bool(b) => b.to_string(),
                Value::String(s) if s == "true" || s == "false" => s.clone(),
                _ => return err_resp(400, "visible must be true or false"),
            },
            "max_agent_count" | "max_work_timeout_sec" | "max_connection_timeout_sec" | "sleep_interval_sec" => {
                match val.as_u64().or_else(|| val.as_str().and_then(|s| s.trim().parse().ok())) {
                    Some(n) => n.to_string(),
                    None => return err_resp(400, &format!("{} must be a non-negative integer", key)),
                }
            }
            _ => match val.as_str() {
                // Frontmatter is one line per key.
                Some(s) => s.replace(['\n', '\r'], " ").trim().to_string(),
                None => return err_resp(400, &format!("{} must be a string", key)),
            },
        };
        updates.push((key.clone(), text));
    }
    let rewritten = crate::agents::apply_updates(&existing, &updates, new_body.as_deref());
    let tmp = path.with_extension("md.tmp");
    if std::fs::write(&tmp, &rewritten).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        return err_resp(500, "cannot write agent file");
    }
    json_resp(200, agent_json(project, &crate::agents::parse(name, &rewritten)))
}

/// PUT /api/projectsettings {server?: {...}, project?: {...}} — the server
/// half lands in `.iter/config.iter.json`; the project half is a record-level
/// frontmatter edit of main.iter.md (the body is never touched here). List
/// values are written in the inline `key: ["a", "b"]` form the parser reads.
fn api_projectsettings_put(req: &Req, project: &Path) -> Resp {
    let Ok(v @ Value::Object(_)) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    if let Some(server) = v.get("server") {
        let Ok(sc) = serde_json::from_value::<crate::project::ServerConfig>(server.clone()) else {
            return err_resp(400, "server settings do not match config.iter.json's shape");
        };
        let path = crate::project::config_path(project);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&sc).unwrap_or_default();
        if std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            return err_resp(500, "cannot write config.iter.json");
        }
    }
    if let Some(Value::Object(proj)) = v.get("project") {
        // Reload AFTER a possible server write: topdir/mainfile may have moved.
        let p = crate::project::Project::load(project);
        if !p.mainfile.is_file() {
            let _ = crate::project::ensure_head_files(project);
        }
        let p = crate::project::Project::load(project);
        for key in [
            "projectname",
            "projectdescription",
            "globalscandirs",
            "globalinterfacedir",
            "globalusecasedir",
            "globalcontextfiles",
        ] {
            let Some(val) = proj.get(key) else { continue };
            let rendered = match val {
                Value::String(s) => {
                    if s.trim().is_empty() { None } else { Some(format!("\"{}\"", s.replace('"', " "))) }
                }
                Value::Array(items) => {
                    let parts: Vec<String> = items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| format!("\"{}\"", s.replace('"', " ")))
                        .collect();
                    Some(format!("[{}]", parts.join(", ")))
                }
                Value::Null => None,
                other => Some(other.to_string()),
            };
            if let Err(e) = markers::set_frontmatter_key(&p.mainfile, key, rendered.as_deref()) {
                return err_resp(500, &format!("cannot update {}: {}", p.mainfile.display(), e));
            }
        }
    }
    json_resp(200, project_settings(project))
}

fn api_markers(project: &Path) -> Resp {
    let (proj, mut scan) = markers::scan_project(project);
    if scan.usecases.is_empty() && seed_starter_usecase(project, &proj) {
        scan = markers::scan(&proj); // pick the seeded file up
    }
    json_resp(200, serde_json::to_value(scan).unwrap_or(Value::Null))
}

/// A project with zero use-cases gets a starter: the getting-started story of this
/// very app. Seeded ONCE (flag file) so deleting it is a real delete, not a respawn.
fn seed_starter_usecase(project: &Path, proj: &crate::project::Project) -> bool {
    let flag = config::engine_dir(project).join("usecases_seeded");
    if flag.exists() {
        return false;
    }
    let dir = &proj.usecasedir;
    let path = dir.join("install-iter-framework.usecase.iter.md");
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let stub = r#"---
name: "Install iter framework"
description: "Getting started: install iterapp, scaffold a project, and run the first loop"
children:
  codenodes: []
  testgroups: ["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"]
---

# Install the iter framework

The getting-started use-case for iterapp itself: from an empty directory to a
running loop with agents picking up work. (This is a seeded starter — edit it,
point its codenodes at your real nodes, or delete it.)

## Steps

1. **Get the binary.** Build from source (`cargo build --release`) or copy a
   built `iter` onto the box. Deploy with a fresh inode (`rm` + `cp`, or
   `cp` + `mv`) — never overwrite a live executable in place.
2. **Scaffold.** `iter init .` (or just `iter start` — missing pieces heal on
   boot) creates `.iter/` (agent personas in `agents/`, pre/post steps in
   `prepostwork/`, engine config in `.engine/config.json`) plus the two
   structureV2 head files: `.iter/config.iter.json` (server settings) and a
   stub `main.iter.md` at the top directory (the project definition, first
   into every agent context).
3. **Describe the project.** Fill in main.iter.md, then add `*.code.iter.md`
   nodes as you go — a `level: context` node attaches to the project head,
   and its `children.codenodes` links attach everything below it.
4. **Tune.** Review `.iter/.engine/config.json` (models, budgets, agent caps)
   and Project Settings in the webapp (scan dirs, context files).
5. **Run.** `iter start` launches the engine plus this webapp and prints the
   URL. `iter stop --wait` drains cleanly.
6. **Feed it.** Create the first work item — from a Projects node, the New
   WorkItem form, or `"$ITER_BIN" add ...` — and watch the loop take it from
   `queued` to `complete`.
"#;
    if std::fs::write(&path, stub).is_err() {
        return false;
    }
    let _ = std::fs::write(&flag, workitems::now_iso());
    true
}

/// Shape a V2 use-case file from its parts. `codenodes` is the REQUIRED link
/// list (may be empty); extra scalar keys the form doesn't own ride through.
fn usecase_file_text(
    name: &str,
    description: &str,
    codenodes: &[String],
    testgroups: &[String],
    body: &str,
    extra: &[(String, String)],
) -> String {
    let clean = |s: &str| s.replace(['"', '\n', '\r'], " ").trim().to_string();
    let mut t = format!("---\nname: \"{}\"\ndescription: \"{}\"\n", clean(name), clean(description));
    for (k, v) in extra {
        // Quote prose values containing ": " so strict-YAML readers keep loading.
        if v.contains(": ") {
            t.push_str(&format!("{}: \"{}\"\n", k, clean(v)));
        } else {
            t.push_str(&format!("{}: {}\n", k, v));
        }
    }
    let render_list = |items: &[String]| {
        let parts: Vec<String> = items
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{}\"", s.replace('"', " ")))
            .collect();
        format!("[{}]", parts.join(", "))
    };
    t.push_str("children:\n");
    t.push_str(&format!("  codenodes: {}\n", render_list(codenodes)));
    if !testgroups.is_empty() {
        t.push_str(&format!("  testgroups: {}\n", render_list(testgroups)));
    }
    t.push_str("---\n\n");
    t.push_str(body.trim_end());
    t.push('\n');
    t
}

/// Validate a client-supplied path for use-case update/delete: must exist, live
/// under the project or code root, and BE a use-case BY FILENAME (`*usecase.iter.md`
/// — the filename declares the role) — this endpoint must not touch other file kinds.
fn usecase_path(project: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("missing file".into());
    }
    let path = PathBuf::from(raw);
    let path = path.canonicalize().map_err(|e| format!("no such file: {}", e))?;
    let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if markers::role_of(&fname) != Some(markers::Role::Usecase) {
        return Err("not a use-case file (the name must end in usecase.iter.md)".into());
    }
    let code_root = config::code_root(project, &config::load(project));
    let proj = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    if !path.starts_with(&proj) && !path.starts_with(&code_root) {
        return Err("file is outside the project".into());
    }
    Ok(path)
}

/// POST /api/usecases (create), PUT /api/usecases (update: body carries `file`),
/// POST /api/usecases/delete. Created use-cases are FOLDERED — the same
/// folder-owns-its-files law C4 objects follow and the usecase agent writes:
/// `<usecase_default_path>/<slug>/<slug>.usecase.iter.md` declaring
/// `testgroup:`/`test_dir:`, plus a starter testgroup.iter.md holding one
/// empty-testlist E2E group (the sweep turns empty testlists into testwriter
/// authoring items, so E2E coverage follows automatically). Updates rewrite
/// the file wherever it lives, carrying through frontmatter keys the form
/// doesn't own (testgroup, test_dir, test_loop, …). Deleting a FOLDERED
/// use-case (`<name>/<name>.usecase.iter.md`) removes the whole folder, tests
/// included; a flat file is removed alone.
fn api_usecases(req: &Req, project: &Path, action: &str) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let s = |k: &str| body.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let list = |k: &str| -> Vec<String> {
        body.get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };
    let codenodes = list("codenodes");
    match action {
        "delete" => match usecase_path(project, &s("file")) {
            Ok(path) => {
                // The foldered signature: the file's own directory is named
                // after it (`<name>/<name>.usecase.iter.md`).
                let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let stem = fname.strip_suffix(".usecase.iter.md").unwrap_or("");
                let folder = path
                    .parent()
                    .filter(|d| !stem.is_empty() && d.file_name().map(|n| n.to_string_lossy() == stem).unwrap_or(false));
                let result = match folder {
                    Some(dir) => std::fs::remove_dir_all(dir).map(|()| dir.to_path_buf()),
                    None => std::fs::remove_file(&path).map(|()| path.clone()),
                };
                match result {
                    Ok(removed) => json_resp(
                        200,
                        json!({ "deleted": removed.to_string_lossy(), "folder": folder.is_some() }),
                    ),
                    Err(e) => err_resp(500, &format!("cannot delete: {}", e)),
                }
            }
            Err(e) => err_resp(400, &e),
        },
        "create" | "update" => {
            let (name, description) = (s("name"), s("description"));
            if name.trim().is_empty() {
                return err_resp(400, "a use-case needs a name");
            }
            let cfg = config::load(project);
            // Frontmatter keys beyond the form's own fields ride through an
            // update untouched (teststate must survive an edit); a create
            // declares its tests from day one.
            let mut extra: Vec<(String, String)> = Vec::new();
            let mut testgroups_link: Vec<String> = Vec::new();
            let path = if action == "update" {
                let path = match usecase_path(project, &s("file")) {
                    Ok(p) => p,
                    Err(e) => return err_resp(400, &e),
                };
                if let Ok(prev) = std::fs::read_to_string(&path) {
                    let front = markers::parse_front(&prev);
                    extra = front
                        .scalars
                        .clone()
                        .into_iter()
                        .filter(|(k, _)| !matches!(k.as_str(), "name" | "description" | "participants" | "test_loop"))
                        .collect();
                    extra.sort();
                    testgroups_link = front.child("testgroups").unwrap_or_default();
                }
                path
            } else {
                let slug: String = name
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect();
                let slug = slug.trim_matches('-').to_string();
                let slug = if slug.is_empty() { "new".to_string() } else { slug };
                let folder = config::usecase_dir(project, &cfg).join(&slug);
                // The filename IS the role: created use-cases are *.usecase.iter.md.
                let p = folder.join(format!("{}.usecase.iter.md", slug));
                if p.exists() {
                    return err_resp(409, &format!("{} already exists — edit it instead", p.display()));
                }
                let test_dir = cfg.globalsettings.test_dir.clone();
                if let Err(e) = std::fs::create_dir_all(folder.join(&test_dir)) {
                    return err_resp(500, &format!("cannot create {}: {}", folder.display(), e));
                }
                // Starter testgroup: one empty-testlist E2E group — enough for
                // the sweep to birth the testwriter authoring item.
                let tg_path = folder.join(&test_dir).join(format!("{}.testgroup.iter.md", slug));
                if !tg_path.exists() {
                    let group = crate::testgroups::TestGroup {
                        label: format!("{}-e2e", slug),
                        desc: format!(
                            "End-to-end journey tests for use-case \"{}\": scripts that walk the actual user journey through the real linked code nodes",
                            name.trim()
                        ),
                        auto_fix: false,
                        ..Default::default()
                    };
                    let header = format!(
                        "---\nname: \"{} E2E tests\"\ndescription: \"End-to-end journey tests for this use-case\"\nchildren:\n  testpaths: [\"{{thisfiledir}}/*.sh\"]\n---\n\n# {} — E2E journey test groups\n\nDefinitions only; the testwriter authors the scripts and fills the testlist.\n",
                        name.trim(),
                        name.trim()
                    );
                    if let Err(e) = std::fs::write(&tg_path, crate::testgroups::update(&header, &[group])) {
                        return err_resp(500, &format!("cannot write {}: {}", tg_path.display(), e));
                    }
                }
                testgroups_link = vec![format!("{{thisfiledir}}/{}/*.testgroup.iter.md", test_dir)];
                p
            };
            match std::fs::write(&path, usecase_file_text(&name, &description, &codenodes, &testgroups_link, &s("body"), &extra)) {
                Ok(()) => json_resp(if action == "create" { 201 } else { 200 }, json!({ "file": path.to_string_lossy() })),
                Err(e) => err_resp(500, &format!("cannot write {}: {}", path.display(), e)),
            }
        }
        _ => err_resp(404, "no such usecase action"),
    }
}

/* -------------------------------------------------- file + testgroups API */

/// Containment check for client-supplied paths: must resolve inside the project
/// root or the code root. `path` must already be canonical.
fn path_contained(project: &Path, path: &Path) -> bool {
    let proj = project.canonicalize().unwrap_or_else(|_| project.to_path_buf());
    let code_root = config::code_root(project, &config::load(project));
    path.starts_with(&proj) || path.starts_with(&code_root)
}

fn body_str<'a>(body: &'a Value, key: &str) -> &'a str {
    body.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// POST /api/orphans/link {orphan, parent} — the Orphanage's quick-link: add
/// the orphaned file to `parent`'s matching `children` sub-key (codenodes for
/// code orphans, testgroups/bizreqs/techreqs for the rest). Parent = node
/// key/name, use-case name, interface id, or declaring-file path suffix.
/// When the sub-key was riding on defaults, the defaults are written out
/// first so existing links survive the edit.
fn api_orphan_link(req: &Req, project: &Path) -> Resp {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let orphan_path = body_str(&body, "orphan").to_string();
    let parent_ref = body_str(&body, "parent").trim().to_string();
    if orphan_path.is_empty() || parent_ref.is_empty() {
        return err_resp(400, "orphan (file path) and parent (node key / usecase name / interface id) are required");
    }
    let (proj, scan) = markers::scan_project(project);
    let Some(orphan) = scan.orphans.iter().find(|o| o.path == orphan_path) else {
        return err_resp(404, "no such orphan (rescan — it may already be linked)");
    };
    // (declaring file, is_code_node, default patterns per sub-key)
    struct ParentRef {
        file: String,
        kind: &'static str,
    }
    let mut parents: Vec<ParentRef> = Vec::new();
    for n in &scan.nodes {
        if n.key == parent_ref || n.name == parent_ref || n.path.ends_with(&parent_ref) {
            parents.push(ParentRef { file: n.path.clone(), kind: "code" });
        }
    }
    for u in &scan.usecases {
        if u.name == parent_ref || u.file.ends_with(&parent_ref) {
            parents.push(ParentRef { file: u.file.clone(), kind: "usecase" });
        }
    }
    for i in &scan.interfaces {
        if i.id == parent_ref || i.file.ends_with(&parent_ref) {
            parents.push(ParentRef { file: i.file.clone(), kind: "interface" });
        }
    }
    parents.dedup_by(|a, b| a.file == b.file);
    let parent = match parents.len() {
        0 => return err_resp(404, "parent matches no node, use case, or interface"),
        1 => parents.pop().expect("one"),
        n => return err_resp(409, &format!("parent is ambiguous ({} matches) — use the node key or file path", n)),
    };
    let (sub_key, default_patterns): (&str, Vec<String>) = match (orphan.role.as_str(), parent.kind) {
        ("code", "code") => ("codenodes", vec![]),
        ("code", _) => ("codenodes", vec![]),
        ("testgroup", "code") => ("testgroups", vec!["{thisfiledir}/test/*.testgroup.iter.md".into()]),
        ("testgroup", _) => ("testgroups", vec!["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md".into()]),
        ("bizreq", "code") => ("bizreqs", vec!["{thisfiledir}/*.bizreq.iter.md".into()]),
        ("bizreq", _) => ("bizreqs", vec!["{thisfiledir}/{thisfilestem}/*.bizreq.iter.md".into()]),
        ("techreq", "code") => ("techreqs", vec!["{thisfiledir}/*.techreq.iter.md".into()]),
        ("techreq", _) => ("techreqs", vec!["{thisfiledir}/{thisfilestem}/*.techreq.iter.md".into()]),
        _ => return err_resp(400, &format!("a {} orphan cannot be linked here", orphan.role)),
    };
    if orphan.role == "code" && parent.kind == "interface" {
        return err_resp(400, "interfaces do not own code nodes — link it under a code node or use case");
    }
    // {topdir}-relative entry keeps the link portable.
    let entry = Path::new(&orphan.path)
        .strip_prefix(&proj.topdir)
        .map(|r| format!("{{topdir}}/{}", r.display()))
        .unwrap_or_else(|_| orphan.path.clone());
    let parent_file = PathBuf::from(&parent.file);
    let front = std::fs::read_to_string(&parent_file)
        .map(|t| markers::parse_front(&t))
        .unwrap_or_default();
    let mut values = front.child(sub_key).unwrap_or(default_patterns);
    if !values.contains(&entry) {
        values.push(entry.clone());
    }
    match markers::set_children_key(&parent_file, sub_key, &values) {
        Ok(()) => json_resp(200, json!({ "linked": entry, "parent": parent.file, "key": sub_key })),
        Err(e) => err_resp(500, &e),
    }
}

/// True when `path` IS the project's main.iter.md — the head file is edited
/// through Settings (frontmatter) or on disk deliberately, never the generic
/// file editor; every other doc stays ordinarily editable.
fn is_head_file(project: &Path, path: &Path) -> bool {
    let main = crate::project::Project::load(project).mainfile;
    main.canonicalize().unwrap_or(main) == path
}

/// POST /api/file/read {path} → {path, content, readonly}. Read-only surface for
/// the UI's lightboxes: markdown (nodes, requirements), run logs, and test
/// scripts. main.iter.md is flagged readonly — the head is edited via Settings.
fn api_file_read(req: &Req, project: &Path) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let raw = body_str(&body, "path");
    let path = match PathBuf::from(raw).canonicalize() {
        Ok(p) => p,
        Err(e) => return err_resp(404, &format!("no such file: {}", e)),
    };
    if !path_contained(project, &path) {
        return err_resp(400, "path is outside the project");
    }
    let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let ok_ext = name.ends_with(".md") || name.ends_with(".log") || name.ends_with(".sh") || name.ends_with(".txt");
    if !ok_ext {
        return err_resp(400, "only .md, .log, .sh, and .txt files are readable here");
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return err_resp(500, &format!("cannot read: {}", e)),
    };
    let readonly = is_head_file(project, &path) || !name.ends_with(".md");
    json_resp(200, json!({ "path": path.to_string_lossy(), "content": content, "readonly": readonly }))
}

/// PUT /api/file {path, content} — markdown only, inside the project/code root,
/// and NEVER main.iter.md (the head is edited through Settings, or on disk
/// deliberately). The file may be new (e.g. a component's first local
/// bizreq file): the parent must exist.
fn api_file_write(req: &Req, project: &Path) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let raw = body_str(&body, "path");
    let Some(content) = body.get("content").and_then(|v| v.as_str()) else {
        return err_resp(400, "missing content");
    };
    if raw.is_empty() {
        return err_resp(400, "missing path");
    }
    let path = PathBuf::from(raw);
    let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.ends_with(".md") {
        return err_resp(400, "only markdown files are writable here");
    }
    let parent = match path.parent().and_then(|p| p.canonicalize().ok()) {
        Some(p) => p,
        None => return err_resp(400, "parent directory does not exist"),
    };
    let path = parent.join(&name);
    if !path_contained(project, &path) {
        return err_resp(400, "path is outside the project");
    }
    if is_head_file(project, &path) {
        return err_resp(403, &format!("main.iter.md is edited through Settings — or edit {} on disk deliberately", path.display()));
    }
    let tmp = path.with_extension("md.tmp");
    if std::fs::write(&tmp, content).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        return err_resp(500, "cannot write file");
    }
    json_resp(200, json!({ "path": path.to_string_lossy() }))
}

/// POST /api/teststate {target, action: omit|include|block|clear} — the
/// webapp's teststate gate toggle, through the same engine-owned edit path as
/// `iter teststate`. Refusals (unknown/ambiguous target, blocked flag) come
/// back as 409 with the reason. (/api/testloop is the V1 alias.)
fn api_teststate(req: &Req, project: &Path) -> Resp {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let Some(target) = body.get("target").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty()) else {
        return err_resp(400, "target (node key/name, use-case name, interface id, or file path) is required");
    };
    let action = match body.get("action").and_then(|a| a.as_str()) {
        Some("omit") => markers::TestStateAction::Omit,
        Some("include") => markers::TestStateAction::Include,
        Some("block") | Some("blocked") => markers::TestStateAction::Block,
        Some("inherit") => markers::TestStateAction::Inherit,
        Some("clear") => markers::TestStateAction::Clear,
        _ => return err_resp(400, "action must be inherit|omit|include|block|clear"),
    };
    // The webapp is the HUMAN surface: lifting a block here is the sanctioned
    // path (the CLI — the agents' path — still refuses).
    let (_p, scan) = markers::scan_project(project);
    match markers::teststate_apply(&scan, target, action, true) {
        Ok(summary) => json_resp(200, json!({ "summary": summary })),
        Err(e) => err_resp(409, &e),
    }
}

/// GET /api/testgroups — DAG-driven, matching the sweep: each node's resolved
/// `children.testgroups` links name its testgroup files. `test_dir` in the
/// response is the directory holding the testgroup.iter.md — where the `runs/`
/// history lives. Unclaimed testgroup files are the Orphanage's.
fn api_testgroups(project: &Path) -> Resp {
    let (_proj, scan) = markers::scan_project(project);
    let ts_json = |state: markers::TestState| -> Value {
        match state {
            markers::TestState::Omitted { value, by } => json!({ "state": "omitted", "value": value, "by": by }),
            markers::TestState::Included => json!({ "state": "included" }),
        }
    };
    let mut files: Vec<Value> = Vec::new();
    // (kind, dir, key, name, level, declaring, teststate, tg files, missing entries)
    let mut rows: Vec<(String, String, String, String, String, String, Value, Vec<String>, Vec<String>)> = Vec::new();
    for n in &scan.nodes {
        rows.push((
            "object".into(),
            n.dir.clone(),
            n.key.clone(),
            n.name.clone(),
            n.level.clone(),
            n.path.clone(),
            ts_json(markers::effective_teststate(n, &scan.nodes)),
            n.testgroups.clone(),
            n.missing_testgroups.clone(),
        ));
    }
    for u in &scan.usecases {
        let dir = Path::new(&u.file).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        rows.push((
            "usecase".into(),
            dir,
            String::new(),
            u.name.clone(),
            "usecase".into(),
            u.file.clone(),
            ts_json(markers::own_teststate(&u.teststate, &u.file)),
            u.testgroups.clone(),
            u.missing_testgroups.clone(),
        ));
    }
    for i in &scan.interfaces {
        let dir = Path::new(&i.file).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        rows.push((
            "interface".into(),
            dir,
            String::new(),
            i.id.clone(),
            "interface".into(),
            i.file.clone(),
            ts_json(markers::own_teststate(&i.teststate, &i.file)),
            i.testgroups.clone(),
            i.missing_testgroups.clone(),
        ));
    }
    let mut undeclared: Vec<Value> = Vec::new();
    for (kind, dir, key, name, level, declaring, ts, tgs, missing) in rows {
        if tgs.is_empty() && missing.is_empty() {
            if kind == "object" {
                undeclared.push(json!({ "c4_name": name, "c4_level": level, "marker": declaring }));
            }
            continue;
        }
        for tg in &tgs {
            let tg_file = PathBuf::from(tg);
            let Ok(text) = std::fs::read_to_string(&tg_file) else { continue };
            files.push(json!({
                "file": tg_file.to_string_lossy(),
                "test_dir": tg_file.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| dir.clone()),
                "kind": kind,
                "c4_dir": dir,
                "c4_key": key,
                "c4_name": name,
                "c4_level": level,
                "marker": declaring,
                "teststate": ts.clone(),
                "groups": testgroups::parse(&text),
            }));
        }
        for m in &missing {
            files.push(json!({
                "file": m,
                "missing": true,
                "kind": kind,
                "c4_dir": dir,
                "c4_key": key,
                "c4_name": name,
                "c4_level": level,
                "marker": declaring,
                "teststate": ts.clone(),
                "groups": [],
            }));
        }
    }
    let orphans: Vec<String> = scan
        .orphans
        .iter()
        .filter(|o| o.role == "testgroup")
        .map(|o| o.path.clone())
        .collect();
    json_resp(200, json!({ "files": files, "orphans": orphans, "undeclared": undeclared }))
}

/// POST /api/testgroups/autofix {file, label, auto_fix} — flip one group's
/// auto-fix gate (queued vs todo for sweep-born fix items).
fn api_testgroups_autofix(req: &Req, project: &Path) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let label = body_str(&body, "label");
    let Some(auto_fix) = body.get("auto_fix").and_then(|v| v.as_bool()) else {
        return err_resp(400, "auto_fix must be true or false");
    };
    let path = match PathBuf::from(body_str(&body, "path")).canonicalize() {
        Ok(p) => p,
        Err(e) => return err_resp(404, &format!("no such file: {}", e)),
    };
    if !path_contained(project, &path)
        || path.file_name().map(|f| markers::role_of(&f.to_string_lossy()) != Some(markers::Role::Testgroup)).unwrap_or(true)
    {
        return err_resp(400, "path must be a *testgroup.iter.md file inside the project");
    }
    let Ok(content) = std::fs::read_to_string(&path) else { return err_resp(500, "cannot read file") };
    let mut groups = testgroups::parse(&content);
    let Some(g) = groups.iter_mut().find(|g| g.label == label) else {
        return err_resp(404, "no such testgroup in this file");
    };
    g.auto_fix = auto_fix;
    let updated = testgroups::update(&content, &groups);
    if std::fs::write(&path, updated).is_err() {
        return err_resp(500, "cannot write file");
    }
    json_resp(200, json!({ "label": label, "auto_fix": auto_fix }))
}

/// POST /api/testruns {dir} → the run-history files under `<dir>/runs/`,
/// newest first (timestamped filenames make time-ordering = name-ordering).
fn api_testruns(req: &Req, project: &Path) -> Resp {
    let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let dir = match PathBuf::from(body_str(&body, "dir")).canonicalize() {
        Ok(p) => p,
        Err(e) => return err_resp(404, &format!("no such directory: {}", e)),
    };
    if !path_contained(project, &dir) {
        return err_resp(400, "dir is outside the project");
    }
    let runs_dir = dir.join("runs");
    let mut rows: Vec<Value> = std::fs::read_dir(&runs_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "log").unwrap_or(false))
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    Some(json!({
                        "name": e.file_name().to_string_lossy(),
                        "path": e.path().to_string_lossy(),
                        "size": meta.len(),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    json_resp(200, json!({ "runs": rows }))
}

/// POST /api/validate {path?, fix?} — the same role-aware checks as `iter
/// validate`. Without `path` every *.iter.md under the scan roots is checked;
/// with `path` just that file (which must sit inside the project or code root).
/// `fix: true` applies the safe mechanical corrections in place.
fn api_validate(req: &Req, project: &Path) -> Resp {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let fix = body.get("fix").and_then(|v| v.as_bool()).unwrap_or(false);
    let single = body.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let single_path = match single {
        Some(raw) => match PathBuf::from(raw).canonicalize() {
            Ok(p) if path_contained(project, &p) => Some(p),
            Ok(_) => return err_resp(400, "path is outside the project"),
            Err(e) => return err_resp(404, &format!("no such file: {}", e)),
        },
        None => None,
    };
    let roots = scan_roots(project);
    match crate::validate::run(&roots, single_path.as_deref(), fix) {
        Ok(report) => json_resp(200, serde_json::to_value(&report).unwrap_or(Value::Null)),
        Err(e) => err_resp(500, &e.to_string()),
    }
}

/* ------------------------------------------------------------ SSE */

/// Change feed: watch the queue + closed-file fingerprints, emit an event on change.
/// The page re-fetches on each event — dumb, robust, and plenty for loopback.
fn sse_events(mut stream: TcpStream, project: &Path) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let open = config::engine_dir(project).join("workitems.jsonl");
    let closed = config::engine_dir(project).join("workitems_closed.jsonl");
    let signal = scheduler::stop_signal_path(project);
    let fingerprint = |p: &Path| {
        std::fs::metadata(p).map(|m| (m.len(), m.modified().ok())).unwrap_or((0, None))
    };
    let mut last = (fingerprint(&open), fingerprint(&closed), signal.exists());
    let mut beats: u32 = 0;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(700));
        let now = (fingerprint(&open), fingerprint(&closed), signal.exists());
        let payload = if now != last {
            last = now;
            beats = 0;
            "data: {\"type\":\"change\"}\n\n".to_string()
        } else {
            beats += 1;
            if beats % 20 != 0 {
                continue;
            }
            ": ping\n\n".to_string()
        };
        if stream.write_all(payload.as_bytes()).is_err() {
            return; // client went away
        }
    }
}

/* ------------------------------------------------------------ locks helper */

/// Used by /api/state consumers later; kept public for the CLI status command too.
#[allow(dead_code)]
pub fn active_locks(project: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(project, &mut found);
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().into_owned();
            if path.is_dir() {
                if name == ".git" || name == "target" || name == "node_modules" {
                    continue;
                }
                collect(&path, out);
            } else if name == locks::CODEPATH_LOCK_NAME {
                out.push(path);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_port_is_deterministic_and_in_range() {
        let dir = std::env::temp_dir().join(format!("iter-port-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (l1, p1) = bind(&dir, None).unwrap();
        assert!((9700..9900).contains(&p1), "port {} outside range", p1);
        drop(l1);
        let (_l2, p2) = bind(&dir, None).unwrap();
        assert_eq!(p1, p2, "same project must hash to the same port");
        let (_l3, p3) = bind(&dir, None).unwrap();
        assert_ne!(p2, p3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_from_dirname_and_settings() {
        let dir = std::env::temp_dir().join(format!("My Project_{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let s = slug(&dir);
        assert!(s.starts_with("my-project-"), "sanitized dirname, got {}", s);
        std::fs::write(crate::project::config_path(&dir), r#"{"url_slug":"pdy-dev"}"#).unwrap();
        assert_eq!(slug(&dir), "pdy-dev");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_settings_reflect_the_two_head_files() {
        let dir = std::env::temp_dir().join(format!("iter-ps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let d = project_settings(&dir);
        assert_eq!(d["server"]["iterglob"], "**/*.iter.md");
        std::fs::write(
            dir.join("main.iter.md"),
            "---\nprojectname: \"X\"\nglobalscandirs: [\"{topdir}/core/\"]\n---\nbody\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("core")).unwrap();
        let d = project_settings(&dir);
        assert_eq!(d["project"]["projectname"], "X");
        assert_eq!(d["project_name"], "X", "legacy alias rides along");
        assert!(d["resolved"]["scandirs"][0].as_str().unwrap().ends_with("core"), "{}", d);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
