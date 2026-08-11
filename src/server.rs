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
    if let Some(s) = project_settings(project_root).get("url_slug").and_then(|s| s.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    let name = project_root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into());
    let cleaned: String =
        name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    cleaned.trim_matches('-').to_string()
}

/// .iter/projects.json with defaults filled in (never errors).
pub fn project_settings(project_root: &Path) -> Value {
    let dirname = project_root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into());
    let cleaned: String =
        dirname.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let mut settings = json!({
        "project_name": dirname,
        "url_slug": cleaned.trim_matches('-'),
        "scan_roots": ["."],
        "marker_glob": "**/*.iter.md",
        "testgroups_glob": "**/testgroups.iter.md",
        "default_context": ["{marker}", "{ancestor_markers}", "{interfaces}"],
    });
    if let Ok(text) = std::fs::read_to_string(project_root.join(".iter/projects.json")) {
        if let Ok(Value::Object(saved)) = serde_json::from_str::<Value>(&text) {
            for (k, v) in saved {
                settings[k] = v;
            }
        }
    }
    settings
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
        ("GET", ["api", "projectsettings"]) => json_resp(200, project_settings(project)),
        ("PUT", ["api", "projectsettings"]) => api_projectsettings_put(req, project),
        ("GET", ["api", "markers"]) | ("POST", ["api", "markers", "rescan"]) => api_markers(project),
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
    json_resp(
        200,
        json!({
            "engine": engine.state(),
            "port": port,
            "project": project.to_string_lossy(),
            "slug": slug(project),
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
    let mut prepost: Vec<String> = std::fs::read_dir(project.join(".iter/prepostwork"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().and_then(|x| x.to_str()) == Some("md"))
                        .then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default();
    prepost.sort();
    let settings = project_settings(project);
    json_resp(
        200,
        json!({
            "agents": agents,
            "prepostwork": prepost,
            "project_root": project.canonicalize().unwrap_or_else(|_| project.to_path_buf()).to_string_lossy(),
            "project_name": settings["project_name"],
            "url_slug": settings["url_slug"],
            "default_context": settings["default_context"],
        }),
    )
}

fn api_list(project: &Path) -> Resp {
    let queue = queue_for(project);
    let mut closed = queue.load_closed();
    if closed.len() > 500 {
        closed = closed.split_off(closed.len() - 500);
    }
    json_resp(200, json!({ "open": queue.load(), "closed": closed }))
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
    if !matches!(item.state.as_str(), "queued" | "todo" | "paused") {
        item.state = workitems::STATE_QUEUED.into();
    }
    let known: Vec<String> = crate::agents::discover(project).into_iter().map(|a| a.type_name).collect();
    let warning = (!known.is_empty() && !known.contains(&item.item_type))
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

const EDITABLE: &[&str] =
    &["title", "type", "priority", "risk", "source", "codepath", "context", "testfiles", "prework", "postwork", "mainwork"];

fn api_patch(req: &Req, project: &Path, id: &str) -> Resp {
    let Ok(Value::Object(patch)) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object of editable fields");
    };
    let queue = queue_for(project);
    let result = queue.with_lock(|items| {
        let Some(item) = items.iter_mut().find(|i| i.workid == id) else {
            return Err((404, "no such open work item".to_string()));
        };
        if item.state != workitems::STATE_TODO && item.state != workitems::STATE_PAUSED {
            return Err((409, format!("only todo/paused items are editable (state: {})", item.state)));
        }
        let mut v = serde_json::to_value(&*item).expect("serializes");
        for (key, val) in &patch {
            if EDITABLE.contains(&key.as_str()) {
                v[key] = val.clone();
            }
        }
        match serde_json::from_value::<WorkItem>(v) {
            Ok(updated) => {
                *item = updated;
                Ok(item.clone())
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
            _ => Err((400, "action must be queue|todo|pause|complete|delete|clone".to_string())),
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
    let codepath = if Path::new(&item.codepath).is_absolute() {
        PathBuf::from(&item.codepath)
    } else {
        project.join(&item.codepath)
    };
    let (files, _warnings) = context::resolve(&item.testfiles, &codepath, project);
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

fn api_projectsettings_put(req: &Req, project: &Path) -> Resp {
    let Ok(v @ Value::Object(_)) = serde_json::from_slice::<Value>(&req.body) else {
        return err_resp(400, "body must be a JSON object");
    };
    let path = project.join(".iter/projects.json");
    if std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default()).is_err() {
        return err_resp(500, "cannot write projects.json");
    }
    json_resp(200, v)
}

fn api_markers(project: &Path) -> Resp {
    let settings = project_settings(project);
    let roots: Vec<PathBuf> = settings["scan_roots"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(expand_root(project)).collect())
        .unwrap_or_else(|| vec![project.to_path_buf()]);
    let glob = settings["marker_glob"].as_str().unwrap_or("**/*.iter.md");
    let scan = markers::scan(project, &roots, glob);
    json_resp(200, serde_json::to_value(scan).unwrap_or(Value::Null))
}

fn expand_root(project: &Path) -> impl Fn(&str) -> PathBuf + '_ {
    move |s: &str| {
        if let Some(rest) = s.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        let p = PathBuf::from(s);
        if p.is_absolute() { p } else { project.join(p) }
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
        std::fs::write(dir.join(".iter/projects.json"), r#"{"url_slug":"pdy-dev"}"#).unwrap();
        assert_eq!(slug(&dir), "pdy-dev");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_settings_defaults_and_overlay() {
        let dir = std::env::temp_dir().join(format!("iter-ps-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let d = project_settings(&dir);
        assert_eq!(d["marker_glob"], "**/*.iter.md");
        std::fs::write(dir.join(".iter/projects.json"), r#"{"project_name":"X","marker_glob":"**/*.x.md"}"#).unwrap();
        let d = project_settings(&dir);
        assert_eq!(d["project_name"], "X");
        assert_eq!(d["marker_glob"], "**/*.x.md");
        assert_eq!(d["testgroups_glob"], "**/testgroups.iter.md", "unset keys keep defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
