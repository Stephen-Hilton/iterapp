//! `iter_data --migrate-v2 <iter.db>`: one-shot import of a V2 project
//! (sqlite `workitems` + `critiques`, the `.iter/agents/*.md` definitions,
//! and the main.iter.md project description) into V3 storage.  Writes go
//! through the Storage trait directly, so this works against sqlite or
//! DynamoDB without a server running.  Idempotent: rows that already exist
//! are skipped unless `overwrite` is set.
//!
//! Field map (V2 -> V3), decided 2026-09-03 for the pdy-dev migration:
//!   workid->id, title->name, type->agent ("exec" when exec=="shell"),
//!   state todo->parked / in-progress->queued (others 1:1), codepaths->
//!   lockdirs with the absolute topdir rewritten to "{topdir}", depends_on->
//!   blockedby, created_by->createdby, source->requestedby, attempts->attempt,
//!   times.added/start/closed -> ts.receive/start/complete, sched as-is.
//!   Detail rows: request=mainwork, question widget (answer field), one
//!   "review" row per critique round, a "v2" json row with every leftover
//!   field, and response=output last.  Priorities are copied unchanged: the
//!   open queue already uses the lower-is-sooner scheme.

use crate::storage::{Storage, StorageError};
use iter_core::now_utc;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const BY: &str = "migrate-v2";

pub struct Options {
    pub db_path: String,
    pub project: String,
    /// absolute V2 topdir, rewritten to "{topdir}" in lockdirs
    pub topdir_abs: String,
    /// V3 engine-side topdir for the engine record (e.g. "~/dev/pdy-dev/")
    pub engine_topdir: String,
    pub engine_name: String,
    pub agents_dir: String,
    pub mainfile: String,
    pub overwrite: bool,
    pub dry_run: bool,
}

#[derive(Default, Debug)]
pub struct Report {
    pub items_written: usize,
    pub items_skipped: usize,
    pub items_by_state: BTreeMap<String, usize>,
    pub details_written: usize,
    pub reviews: usize,
    pub agents_written: usize,
    pub agents_skipped: usize,
    pub project_written: bool,
    pub engine_written: bool,
    pub user_written: Option<String>,
    pub warnings: Vec<String>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn arr(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

pub fn map_state(v2: &str) -> String {
    match v2 {
        "todo" => "parked".into(),
        "in-progress" => "queued".into(),
        other if iter_core::STATES.contains(&other) => other.into(),
        _ => "parked".into(),
    }
}

/// "/abs/topdir/src/x" -> "{topdir}/src/x"; the topdir itself -> "{topdir}/".
pub fn map_path(p: &str, topdir_abs: &str) -> String {
    let top = topdir_abs.trim_end_matches('/');
    let p = p.trim();
    if p.is_empty() {
        return String::new();
    }
    if p == top {
        return "{topdir}/".into();
    }
    if let Some(rest) = p.strip_prefix(top) {
        if rest.starts_with('/') {
            return format!("{{topdir}}{rest}");
        }
    }
    if p.starts_with("{topdir}") {
        return p.to_string();
    }
    if !p.starts_with('/') && !p.starts_with('~') {
        // V2 also stored repo-relative codepaths ("core/repos/x")
        return format!("{{topdir}}/{}", p.trim_start_matches("./"));
    }
    p.to_string()
}

/// Build the V3 workitem body from a V2 body.
pub fn map_item(v2: &Value, project: &str, topdir_abs: &str) -> Value {
    let is_exec = s(v2, "exec") == "shell";
    let mut lockdirs: Vec<String> = arr(v2, "codepaths");
    if lockdirs.is_empty() && !s(v2, "codepath").is_empty() {
        lockdirs.push(s(v2, "codepath"));
    }
    let lockdirs: Vec<String> =
        lockdirs.iter().map(|p| map_path(p, topdir_abs)).filter(|p| !p.is_empty()).collect();
    let exec_shell = if is_exec {
        let mut lines = arr(v2, "prework");
        lines.push(s(v2, "mainwork"));
        lines.extend(arr(v2, "postwork"));
        lines.into_iter().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
    } else {
        String::new()
    };
    let times = v2.get("times").cloned().unwrap_or(json!({}));
    let mut tags: Vec<Value> = Vec::new();
    let todo_reason = s(v2, "todo_reason");
    if !todo_reason.is_empty() {
        tags.push(json!({"text": format!("todo:{todo_reason}"), "color": "#d9cf8e"}));
    }
    let mut body = json!({
        "id": s(v2, "workid"),
        "project": project,
        "version": 1,
        "name": s(v2, "title"),
        "state": map_state(&s(v2, "state")),
        "agent": if is_exec { "exec".to_string() } else { s(v2, "type") },
        "exec_shell": exec_shell,
        "priority": v2.get("priority").and_then(|p| p.as_i64()).unwrap_or(5),
        "lockdirs": lockdirs,
        "createdby": s(v2, "created_by"),
        "requestedby": s(v2, "source"),
        "blockedby": arr(v2, "depends_on"),
        "attempt": v2.get("attempts").and_then(|a| a.as_u64()).unwrap_or(0),
        "gate_bounces": 0,
        "prework": [],
        "postwork": [],
        "ts": {"receive": s(&times, "added"), "start": s(&times, "start"), "complete": s(&times, "closed")},
        "tags": tags,
        "source_schedule": s(v2, "source_schedule"),
        "engine": "",
        "lasterror": s(v2, "lasterror"),
        "approval_code": "",
        "needs_approval": false,
    });
    if let Some(sched) = v2.get("sched") {
        if sched.get("kind").and_then(|k| k.as_str()).map(|k| !k.is_empty()).unwrap_or(false) {
            body["sched"] = sched.clone();
        }
    }
    body
}

/// The detail rows for one item, in order.  `critiques` are the V2 rounds.
pub fn map_details(v2: &Value, critiques: &[Value], now: &str) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let mut push = |key: &str, valuetype: &str, value: Value| {
        rows.push(json!({"key": key, "valuetype": valuetype, "value": value, "by": BY, "ts": now}));
    };
    push("request", "text", json!(s(v2, "mainwork")));
    let question = s(v2, "question");
    if !question.trim().is_empty() {
        let title: String = question.lines().next().unwrap_or("").chars().take(150).collect();
        push(
            "question",
            "json",
            json!({
                "title": if title.trim().is_empty() { "Question".to_string() } else { title },
                "summary": "",
                "detail": question,
                "fields": [{"key": "answer", "label": "Answer", "type": "text", "value": s(v2, "answer")}]
            }),
        );
    }
    for c in critiques {
        push(
            "review",
            "json",
            json!({
                "round": c.get("round").cloned().unwrap_or(json!(1)),
                "persona": s(c, "persona"),
                "agent_type": s(c, "agent_type"),
                "critique": s(c, "critique"),
                "disposition": s(c, "disposition"),
                "material": s(c, "material").chars().take(60_000).collect::<String>(),
                "created_at": s(c, "created_at"),
            }),
        );
    }
    // everything V3 has no field for, kept verbatim
    let mut leftovers = serde_json::Map::new();
    for k in [
        "risk", "automation", "model", "context", "testfiles", "source_testgroup", "source_tests",
        "codepath_ignore", "git_start_commit", "todo_reason", "depends_on_shallow", "prework",
        "postwork", "exec", "codepath", "codepaths", "source", "type", "state", "answer",
    ] {
        if let Some(v) = v2.get(k) {
            let keep = match v {
                Value::String(x) => !x.is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Bool(b) => *b,
                Value::Null => false,
                _ => true,
            };
            if keep {
                leftovers.insert(k.to_string(), v.clone());
            }
        }
    }
    push("v2", "json", Value::Object(leftovers));
    let output = s(v2, "output");
    if !output.trim().is_empty() {
        push("response", "text", json!(output));
    }
    rows
}

/// Parse a V2 agent .md: `---` frontmatter (key: value lines) + body.
pub fn parse_agent_md(text: &str) -> (BTreeMap<String, String>, String) {
    let mut fm = BTreeMap::new();
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    fm.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
                }
            }
            let body = rest[end + 4..].trim_start_matches('\n').to_string();
            return (fm, body);
        }
    }
    (fm, text.to_string())
}

fn agent_row(name: &str, fm: &BTreeMap<String, String>, body: &str, shared: &str) -> Value {
    let num = |k: &str| fm.get(k).and_then(|v| v.parse::<u64>().ok());
    let mut promptbody = body.trim_end().to_string();
    if !shared.trim().is_empty() {
        promptbody.push_str("\n\n");
        promptbody.push_str(shared.trim_end());
    }
    json!({
        "name": name,
        "desc": fm.get("description").cloned().unwrap_or_default(),
        "max": num("max_agent_count").unwrap_or(2),
        "childstate": "queued",
        "timeoutsec": num("max_work_timeout_sec").unwrap_or(3600),
        "model": fm.get("model").cloned().unwrap_or_default(),
        "flags": fm.get("model_flags").cloned().unwrap_or_default(),
        "promptbody": promptbody,
    })
}

fn detail_sk(order: i64) -> String {
    format!("{order:010}")
}

async fn exists(store: &dyn Storage, table: &str, pk: &str, sk: &str) -> Result<bool, StorageError> {
    Ok(store.get(table, pk, sk).await?.is_some())
}

pub async fn run(store: &dyn Storage, opts: &Options) -> Result<Report, String> {
    let mut rep = Report::default();
    let now = now_utc();
    let conn = Connection::open_with_flags(&opts.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open {}: {e}", opts.db_path))?;

    // critiques grouped by workid
    let mut crit: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    {
        let mut st = conn
            .prepare("SELECT workid, round, persona, agent_type, material, critique, disposition, created_at FROM critiques ORDER BY workid, round, id")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok(json!({
                    "workid": r.get::<_, String>(0)?, "round": r.get::<_, i64>(1)?,
                    "persona": r.get::<_, String>(2)?, "agent_type": r.get::<_, String>(3)?,
                    "material": r.get::<_, String>(4)?, "critique": r.get::<_, String>(5)?,
                    "disposition": r.get::<_, String>(6)?, "created_at": r.get::<_, String>(7)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        for r in rows.flatten() {
            crit.entry(s(&r, "workid")).or_default().push(r);
        }
    }

    // workitems
    let bodies: Vec<String> = {
        let mut st = conn.prepare("SELECT body FROM workitems ORDER BY seq, rowid").map_err(|e| e.to_string())?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    for b in &bodies {
        let v2: Value = match serde_json::from_str(b) {
            Ok(v) => v,
            Err(e) => {
                rep.warnings.push(format!("unparseable V2 body skipped: {e}"));
                continue;
            }
        };
        let item = map_item(&v2, &opts.project, &opts.topdir_abs);
        let id = s(&item, "id");
        if id.is_empty() {
            rep.warnings.push("V2 item without workid skipped".into());
            continue;
        }
        let state = s(&item, "state");
        if !opts.overwrite && exists(store, "workitem", &opts.project, &id).await.map_err(|e| e.to_string())? {
            rep.items_skipped += 1;
            continue;
        }
        let details = map_details(&v2, crit.get(&id).map(|v| v.as_slice()).unwrap_or(&[]), &now);
        *rep.items_by_state.entry(state).or_default() += 1;
        rep.reviews += crit.get(&id).map(|v| v.len()).unwrap_or(0);
        if opts.dry_run {
            rep.items_written += 1;
            rep.details_written += details.len();
            continue;
        }
        store.put("workitem", &opts.project, &id, &item).await.map_err(|e| e.to_string())?;
        for (i, mut d) in details.into_iter().enumerate() {
            d["id"] = json!(id);
            d["order"] = json!(i as i64);
            store.put("workitem_detail", &id, &detail_sk(i as i64), &d).await.map_err(|e| e.to_string())?;
            rep.details_written += 1;
        }
        rep.items_written += 1;
    }

    // agents: every non-underscore .md; _shared.md appended to each promptbody
    if !opts.agents_dir.is_empty() {
        let shared = std::fs::read_to_string(format!("{}/_shared.md", opts.agents_dir.trim_end_matches('/'))).unwrap_or_default();
        let mut entries: Vec<_> = std::fs::read_dir(&opts.agents_dir)
            .map_err(|e| format!("agents dir {}: {e}", opts.agents_dir))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let fname = e.file_name().to_string_lossy().to_string();
            let Some(name) = fname.strip_suffix(".md") else { continue };
            if name.starts_with('_') {
                continue;
            }
            if !opts.overwrite && exists(store, "agent", name, "-").await.map_err(|e| e.to_string())? {
                rep.agents_skipped += 1;
                continue;
            }
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            let (fm, body) = parse_agent_md(&text);
            let row = agent_row(name, &fm, &body, &shared);
            if !opts.dry_run {
                store.put("agent", name, "-", &row).await.map_err(|e| e.to_string())?;
            }
            rep.agents_written += 1;
        }
    }

    // project record (desc from main.iter.md frontmatter when available)
    let mut desc = format!("{} (migrated from iter V2 on {})", opts.project, &now[..10]);
    if !opts.mainfile.is_empty() {
        if let Ok(text) = std::fs::read_to_string(&opts.mainfile) {
            let (fm, _) = parse_agent_md(&text);
            let pname = fm.get("projectname").cloned().unwrap_or_default();
            let pdesc = fm.get("projectdescription").cloned().unwrap_or_default();
            if !pdesc.is_empty() {
                desc = if pname.is_empty() { pdesc } else { format!("{pname}: {pdesc}") };
            }
        }
    }
    let project_exists = exists(store, "project", &opts.project, "-").await.map_err(|e| e.to_string())?;
    if opts.overwrite || !project_exists {
        let row = json!({
            "name": opts.project, "desc": desc, "state": "Stopped", "gitrepo": "",
            "maxagents": {">95%": 0, ">90%": 1, "else": 1},
            "maxdailycost": null, "agents": {},
            "failure": {"maxattempts": 1, "first_retry_second": 300, "retry_backoff_exponent": 2},
            "engines": [opts.engine_name], "accounts": []
        });
        if !opts.dry_run {
            store.put("project", &opts.project, "-", &row).await.map_err(|e| e.to_string())?;
        }
        rep.project_written = true;
    }
    // engine record: create, or merge this project's dirs into the existing one
    {
        let host = std::process::Command::new("hostname").output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let mut row = store.get("engine", &opts.engine_name, "-").await.map_err(|e| e.to_string())?.unwrap_or(json!({
            "name": opts.engine_name, "host": host, "state": "Stopped", "last_seen": "",
            "ticksec": 5, "full_refresh_minutes": 360, "account": "",
            "queuelock": {"retryms": 50, "breaksec": 60}, "projects": {}
        }));
        if !row["projects"].is_object() {
            row["projects"] = json!({});
        }
        row["projects"][&opts.project] = json!({"dirs": {"topdir": opts.engine_topdir}});
        if !opts.dry_run {
            store.put("engine", &opts.engine_name, "-", &row).await.map_err(|e| e.to_string())?;
        }
        rep.engine_written = true;
    }

    // operator user from ITER_USERNAME / ITER_PASSWORD (admin on this project)
    if let (Ok(u), Ok(p)) = (std::env::var("ITER_USERNAME"), std::env::var("ITER_PASSWORD")) {
        let (u, p) = (u.trim().to_string(), p.trim().to_string());
        if !u.is_empty() && !p.is_empty() {
            let mut row = store.get("webui_user", &u, "-").await.map_err(|e| e.to_string())?.unwrap_or(json!({
                "user": u, "email": "", "role": "admin", "tokenver": 1, "css": "", "pubkey": "", "settings": {}, "authz": {}
            }));
            row["pwhash"] = json!(crate::auth::hash_password(&p)?);
            row["role"] = json!("admin");
            row["authz"][&opts.project] = json!("admin");
            if !opts.dry_run {
                store.put("webui_user", &u, "-", &row).await.map_err(|e| e.to_string())?;
            }
            rep.user_written = Some(u);
        }
    }

    if !opts.dry_run {
        for t in ["workitem", "workitem_detail"] {
            store.bump_seq(&opts.project, t).await.map_err(|e| e.to_string())?;
        }
        for t in ["agent", "project", "engine", "webui_user"] {
            store.bump_seq(crate::api::GLOBAL, t).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_states_paths_and_items() {
        assert_eq!(map_state("todo"), "parked");
        assert_eq!(map_state("in-progress"), "queued");
        assert_eq!(map_state("question"), "question");
        assert_eq!(map_state("bogus"), "parked");
        assert_eq!(map_path("/u/dev/pdy", "/u/dev/pdy/"), "{topdir}/");
        assert_eq!(map_path("/u/dev/pdy/core/x", "/u/dev/pdy"), "{topdir}/core/x");
        assert_eq!(map_path("/elsewhere", "/u/dev/pdy"), "/elsewhere");
        assert_eq!(map_path("core/repos/x", "/u/dev/pdy"), "{topdir}/core/repos/x");
        assert_eq!(map_path("./core", "/u/dev/pdy"), "{topdir}/core");
        assert_eq!(map_path("{topdir}/core", "/u/dev/pdy"), "{topdir}/core");
        let v2 = json!({
            "workid": "w1", "title": "t", "type": "plan", "state": "todo", "source": "user",
            "priority": 2, "risk": 7, "codepaths": ["/u/dev/pdy/core"], "depends_on": ["w0"],
            "created_by": "plan", "attempts": 1, "mainwork": "do it", "output": "done",
            "question": "Which?\nmore", "answer": "A", "todo_reason": "guard",
            "times": {"added": "2026-08-01T00:00:00Z", "start": "", "closed": ""},
            "sched": {"kind": "", "every_min": 0, "at": "", "day": "", "tz": "", "last_fired": ""}
        });
        let item = map_item(&v2, "p", "/u/dev/pdy");
        assert_eq!(item["state"], "parked");
        assert_eq!(item["agent"], "plan");
        assert_eq!(item["lockdirs"][0], "{topdir}/core");
        assert_eq!(item["blockedby"][0], "w0");
        assert_eq!(item["attempt"], 1);
        assert_eq!(item["ts"]["receive"], "2026-08-01T00:00:00Z");
        assert!(item.get("sched").is_none(), "empty sched dropped");
        assert_eq!(item["tags"][0]["text"], "todo:guard");
        let parsed: iter_core::WorkItem = serde_json::from_value(item).unwrap();
        assert_eq!(parsed.priority, 2);
        let det = map_details(&v2, &[json!({"round": 1, "critique": "meh", "disposition": ""})], "now");
        let keys: Vec<&str> = det.iter().map(|d| d["key"].as_str().unwrap()).collect();
        assert_eq!(keys, vec!["request", "question", "review", "v2", "response"]);
        assert!(iter_core::widget::validate(&det[1]["value"]).is_empty());
        assert_eq!(det[1]["value"]["fields"][0]["value"], "A");
        assert_eq!(det[3]["value"]["risk"], 7);
        // exec:shell folds pre/main/post into exec_shell
        let sh = json!({"workid": "w2", "title": "s", "type": "code", "exec": "shell", "state": "paused",
            "prework": ["echo a"], "mainwork": "echo b", "postwork": [], "times": {}});
        let item = map_item(&sh, "p", "/x");
        assert_eq!(item["agent"], "exec");
        assert_eq!(item["exec_shell"], "echo a\necho b");
    }

    #[test]
    fn parses_agent_frontmatter() {
        let (fm, body) = parse_agent_md("---\ndescription: The coder\nmax_agent_count: 5\nmodel: opus\nmodel_flags: --x\n---\n\n# Agent\nbody");
        assert_eq!(fm["description"], "The coder");
        assert!(body.starts_with("# Agent"));
        let row = agent_row("code", &fm, &body, "shared rules");
        assert_eq!(row["max"], 5);
        assert_eq!(row["flags"], "--x");
        assert!(row["promptbody"].as_str().unwrap().ends_with("shared rules"));
    }
}
