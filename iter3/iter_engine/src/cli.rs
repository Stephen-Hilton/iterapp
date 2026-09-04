//! `iter <verb>` for agents (decided 2026-09-04): the same logical verbs V2
//! gave agents (add, ask, reject, critreview, status, plus `doc` and
//! `capability`), backed by iter_data.  Agents ask for the logical thing; this
//! does the deterministic data work.  Verbs that operate on local files
//! (runtests, validate, markers, teststate, usecase, resolve, orphans) are
//! delegated to the V2 binary when the repo still carries it — see
//! `work::v2_delegate` — until V3 re-implements them.
//!
//! Environment (set by the engine for every agent session): ITER_DATA_URL,
//! ITER_ENGINE_TOKEN, ITER_PROJECT (name), ITER_WORKID, ITER_AGENT, ITER_TOPDIR.

use crate::client::Api;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Args, Debug)]
pub struct CliArgs {
    /// project name (defaults to $ITER_PROJECT; a V2-style path is ignored)
    #[arg(long, global = true)]
    project: Option<String>,
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand, Debug)]
enum Verb {
    /// Create a work item (child of $ITER_WORKID when run inside an agent).
    /// Either --file <json> (V2 or V3 field names) or the flags below.
    Add {
        /// JSON file describing the item (title/name, type/agent, mainwork/request,
        /// codepath[s]/lockdirs, depends_on/blockedby, context, priority, model, question)
        #[arg(long)]
        file: Option<String>,
        /// agent type (code | plan | testwriter | …)
        #[arg(long = "type", alias = "agent")]
        item_type: Option<String>,
        #[arg(long, alias = "name")]
        title: Option<String>,
        /// the request text; @path reads a file
        #[arg(long, alias = "request")]
        mainwork: Option<String>,
        /// lock scope (repeatable); absolute, {topdir}-relative or repo-relative
        #[arg(long)]
        codepath: Vec<String>,
        /// lower = sooner, P0 most urgent, default 5
        #[arg(long)]
        priority: Option<i64>,
        /// this item waits for the named item (id or unique suffix) AND everything it created (repeatable)
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
        /// wait for the named items' own completion only
        #[arg(long = "depends-on-shallow", default_value_t = false)]
        depends_on_shallow: bool,
        /// context file patterns for the new item (repeatable)
        #[arg(long)]
        context: Vec<String>,
        /// model override: opus | sonnet | haiku | fable
        #[arg(long)]
        model: Option<String>,
        /// raise it as a QUESTION for the human instead of runnable work
        #[arg(long)]
        question: Option<String>,
        /// tag (repeatable): text or text:#hex
        #[arg(long)]
        tag: Vec<String>,
        /// accepted for V2 compatibility; ignored
        #[arg(long, hide = true)]
        risk: Option<i64>,
        #[arg(long = "source-testgroup", hide = true)]
        source_testgroup: Option<String>,
        #[arg(long, hide = true)]
        automation: Option<String>,
        #[arg(long = "codepath-ignore", hide = true)]
        codepath_ignore: Vec<String>,
    },
    /// Ask the human a question from inside a running work item: the CALLING
    /// item moves to `question` when this turn ends and queues again once answered.
    Ask {
        #[arg(long)]
        question: Option<String>,
        /// read the question from a file (for anything multi-paragraph)
        #[arg(long)]
        file: Option<String>,
    },
    /// Reject the CALLING work item as invalid: it moves to `parked` with the
    /// reason recorded so a human re-evaluates; no retries are burned.
    Reject {
        #[arg(long)]
        reason: String,
    },
    /// Append a "doc" note to a work item (the calling one by default; works on closed items).
    Doc {
        text: Option<String>,
        #[arg(long)]
        file: Option<String>,
        /// another item's id or unique suffix
        #[arg(long)]
        id: Option<String>,
    },
    /// Synchronous critical review by the `_critic` persona; prints its
    /// feedback and records the round as a "review" row. Run again with
    /// --disposition to report what you did with it.
    Critreview {
        /// the material to review (plan text, change summary, …)
        #[arg(long)]
        file: Option<String>,
        /// context file the critic should also read (repeatable)
        #[arg(long)]
        context: Vec<String>,
        #[arg(long = "max-retry", default_value_t = 1)]
        max_retry: u32,
        /// revised | rejected | no-findings
        #[arg(long)]
        disposition: Option<String>,
        /// which round --disposition refers to (default: latest)
        #[arg(long)]
        round: Option<i64>,
    },
    /// Read a capability doc (no name: list them).
    Capability {
        name: Option<String>,
    },
    /// Open work for this project, run-order first.
    Status,
    /// Local-file verbs delegated to the V2 binary (runtests, validate, markers, teststate, usecase, resolve, orphans, …).
    #[command(external_subcommand)]
    Other(Vec<String>),
}

struct Env {
    api: Api,
    project: String,
    workid: String,
    agent: String,
    topdir: String,
}

fn env(args: &CliArgs) -> Env {
    let url = std::env::var("ITER_DATA_URL").unwrap_or_default();
    let token = std::env::var("ITER_ENGINE_TOKEN").unwrap_or_default();
    if url.is_empty() || token.is_empty() {
        eprintln!("iter: ITER_DATA_URL / ITER_ENGINE_TOKEN are not set — this verb only works inside an engine-run work item");
        std::process::exit(2);
    }
    let mut project = std::env::var("ITER_PROJECT").unwrap_or_default();
    if let Some(p) = &args.project {
        if project.is_empty() && !p.contains('/') {
            project = p.clone();
        }
    }
    if project.is_empty() {
        eprintln!("iter: ITER_PROJECT is not set");
        std::process::exit(2);
    }
    Env {
        api: Api::new(&url, &token),
        project,
        workid: std::env::var("ITER_WORKID").unwrap_or_default(),
        agent: std::env::var("ITER_AGENT").unwrap_or_default(),
        topdir: std::env::var("ITER_TOPDIR").unwrap_or_default(),
    }
}

fn die(msg: String) -> ! {
    eprintln!("iter: {msg}");
    std::process::exit(1)
}

fn read_arg_or_file(text: Option<String>, file: Option<String>) -> String {
    if let Some(t) = text {
        if let Some(path) = t.strip_prefix('@') {
            return std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("cannot read {path}: {e}")));
        }
        return t;
    }
    if let Some(path) = file {
        if path == "-" {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).ok();
            return s;
        }
        return std::fs::read_to_string(&path).unwrap_or_else(|e| die(format!("cannot read {path}: {e}")));
    }
    String::new()
}

fn items(e: &Env) -> Vec<Value> {
    e.api.get(&format!("/api/projects/{}/workitems", e.project)).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// A workid or any unambiguous suffix (V2 convention: the last 12 chars).
fn resolve_id(e: &Env, all: &[Value], needle: &str) -> String {
    let n = needle.trim();
    let hits: Vec<String> = all
        .iter()
        .filter_map(|i| i.get("id").and_then(|x| x.as_str()))
        .filter(|id| *id == n || id.ends_with(n) || id.starts_with(n) || id.replace('-', "").ends_with(&n.replace('-', "")))
        .map(String::from)
        .collect();
    match hits.len() {
        1 => hits[0].clone(),
        0 => die(format!("no work item in '{}' matches '{n}'", e.project)),
        k => die(format!("'{n}' is ambiguous ({k} matches) — use more of the id")),
    }
}

/// absolute / {topdir}-relative / repo-relative -> "{topdir}/…"
fn lockdir(p: &str, topdir: &str) -> String {
    let top = topdir.trim_end_matches('/');
    let p = p.trim();
    if p.starts_with("{topdir}") {
        return p.to_string();
    }
    if !top.is_empty() {
        if p == top {
            return "{topdir}/".into();
        }
        if let Some(rest) = p.strip_prefix(top) {
            if rest.starts_with('/') {
                return format!("{{topdir}}{rest}");
            }
        }
    }
    if p.starts_with('/') || p.starts_with('~') {
        return p.to_string();
    }
    format!("{{topdir}}/{}", p.trim_start_matches("./"))
}

fn question_widget(question: &str) -> Value {
    let title: String = question.lines().find(|l| !l.trim().is_empty()).unwrap_or("Question").chars().take(150).collect();
    json!({
        "title": title,
        "summary": "",
        "detail": question,
        "fields": [{"key": "answer", "label": "Answer", "type": "text", "value": ""}]
    })
}

pub fn run(args: CliArgs) {
    match args.verb {
        Verb::Capability { ref name } => capability(&env(&args), name.clone()),
        Verb::Status => status(&env(&args)),
        Verb::Other(ref rest) => delegate(rest.clone()),
        _ => {}
    }
    let e = env(&args);
    match args.verb {
        Verb::Add { file, item_type, title, mainwork, codepath, priority, depends_on, depends_on_shallow, context, model, question, tag, .. } => {
            add(&e, file, item_type, title, mainwork, codepath, priority, depends_on, depends_on_shallow, context, model, question, tag)
        }
        Verb::Ask { question, file } => ask(&e, read_arg_or_file(question, file)),
        Verb::Reject { reason } => reject(&e, &reason),
        Verb::Doc { text, file, id } => doc(&e, read_arg_or_file(text, file), id),
        Verb::Critreview { file, context, max_retry, disposition, round } => critreview(&e, file, context, max_retry, disposition, round),
        Verb::Capability { .. } | Verb::Status | Verb::Other(_) => {}
    }
}

fn add(
    e: &Env,
    file: Option<String>,
    item_type: Option<String>,
    title: Option<String>,
    mainwork: Option<String>,
    codepath: Vec<String>,
    priority: Option<i64>,
    depends_on: Vec<String>,
    depends_on_shallow: bool,
    context: Vec<String>,
    model: Option<String>,
    question: Option<String>,
    tag: Vec<String>,
) {
    let s = |v: &Value, keys: &[&str]| -> String {
        keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()).map(String::from)).unwrap_or_default()
    };
    let arr = |v: &Value, keys: &[&str]| -> Vec<String> {
        keys.iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_array()))
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let f: Value = match &file {
        Some(path) => serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| die(format!("cannot read {path}: {e}"))))
            .unwrap_or_else(|e| die(format!("{path} is not valid json: {e}"))),
        None => json!({}),
    };
    let name = title.clone().unwrap_or_else(|| s(&f, &["title", "name"]));
    if name.trim().is_empty() {
        die("a title is required (--title or \"title\" in --file)".into());
    }
    let agent = item_type.clone().unwrap_or_else(|| s(&f, &["type", "agent"]));
    let agent = if agent.is_empty() { "code".to_string() } else { agent };
    let request = read_arg_or_file(mainwork.clone(), None);
    let request = if request.is_empty() { s(&f, &["mainwork", "request"]) } else { request };
    let mut lockdirs: Vec<String> = codepath.iter().map(|c| lockdir(c, &e.topdir)).collect();
    if lockdirs.is_empty() {
        let mut cps = arr(&f, &["codepaths", "lockdirs"]);
        let single = s(&f, &["codepath"]);
        if cps.is_empty() && !single.is_empty() {
            cps.push(single);
        }
        lockdirs = cps.iter().map(|c| lockdir(c, &e.topdir)).collect();
    }
    let all = items(e);
    let mut blockedby: Vec<String> = depends_on.iter().map(|d| resolve_id(e, &all, d)).collect();
    if blockedby.is_empty() {
        blockedby = arr(&f, &["depends_on", "blockedby"]).iter().map(|d| resolve_id(e, &all, d)).collect();
    }
    if blockedby.iter().any(|b| b == &e.workid) {
        die("an item cannot depend on the item that creates it".into());
    }
    let shallow = depends_on_shallow || f.get("depends_on_shallow").and_then(|b| b.as_bool()).unwrap_or(false) || f.get("blockedby_shallow").and_then(|b| b.as_bool()).unwrap_or(false);
    let ctx = if context.is_empty() { arr(&f, &["context"]) } else { context.clone() };
    let question = question.clone().or_else(|| { let q = s(&f, &["question"]); if q.is_empty() { None } else { Some(q) } });
    let model = model.clone().unwrap_or_else(|| s(&f, &["model"]));
    let prio = priority.or_else(|| f.get("priority").and_then(|p| p.as_i64())).unwrap_or(5);
    let mut tags: Vec<Value> = tag
        .iter()
        .map(|t| match t.rsplit_once(':') {
            Some((text, color)) if color.starts_with('#') => json!({"text": text.trim(), "color": color}),
            _ => json!({"text": t.trim(), "color": ""}),
        })
        .collect();
    if let Some(a) = f.get("tags").and_then(|t| t.as_array()) {
        tags.extend(a.iter().cloned());
    }

    // birth state: a question parks; otherwise the parent agent's childstate
    // (project override first), default queued
    let state = if question.is_some() {
        "question".to_string()
    } else {
        let project: Value = e.api.get(&format!("/api/projects/{}", e.project)).unwrap_or(json!({}));
        let over = project.get("agents").and_then(|a| a.get(&e.agent)).and_then(|o| o.get("childstate")).and_then(|c| c.as_str()).map(String::from);
        let def = e.api.get(&format!("/api/agents/{}", e.agent)).ok().and_then(|a| a.get("childstate").and_then(|c| c.as_str()).map(String::from));
        over.or(def).filter(|c| !c.is_empty()).unwrap_or_else(|| "queued".into())
    };
    let requestedby = if e.agent.is_empty() { "user".to_string() } else { format!("agent:{}", e.agent) };
    let body = json!({
        "name": name.trim(), "agent": agent, "state": state, "priority": prio,
        "lockdirs": lockdirs, "blockedby": blockedby, "blockedby_shallow": shallow,
        "context": ctx, "model": model, "tags": tags,
        "createdby": if e.workid.is_empty() { requestedby.clone() } else { e.workid.clone() },
        "requestedby": requestedby, "prework": [], "postwork": [],
    });
    let created = e.api.post(&format!("/api/projects/{}/workitems", e.project), &body).unwrap_or_else(|err| die(format!("create failed: {err}")));
    let id = created.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
    let _ = e.api.put(
        &format!("/api/projects/{}/workitems/{}/details/0", e.project, id),
        &json!({"key": "request", "valuetype": "text", "value": request}),
    );
    if let Some(q) = question {
        let _ = e.api.post(
            &format!("/api/projects/{}/workitems/{}/details", e.project, id),
            &json!({"key": "question", "valuetype": "json", "value": question_widget(&q)}),
        );
    }
    println!("added {} ({}) state={} agent={}", id, &id[id.len().saturating_sub(12)..], body["state"].as_str().unwrap_or(""), body["agent"].as_str().unwrap_or(""));
}

fn calling_item(e: &Env) -> Value {
    if e.workid.is_empty() {
        die("ITER_WORKID is not set — this verb only works inside an engine-run work item".into());
    }
    e.api.get(&format!("/api/projects/{}/workitems/{}", e.project, e.workid)).unwrap_or_else(|err| die(format!("cannot load the calling item: {err}")))
}

fn set_state(e: &Env, mut item: Value, state: &str, note: Option<&str>) {
    let version = item.get("version").and_then(|v| v.as_u64()).unwrap_or(1);
    item["state"] = json!(state);
    if let Some(n) = note {
        item["lasterror"] = json!(n);
    }
    e.api
        .put(&format!("/api/projects/{}/workitems/{}?expect_version={}", e.project, e.workid, version), &item)
        .unwrap_or_else(|err| die(format!("state change failed: {err}")));
}

fn ask(e: &Env, question: String) {
    if question.trim().is_empty() {
        die("the question is empty (--question or --file)".into());
    }
    let item = calling_item(e);
    let _ = e.api
        .post(
            &format!("/api/projects/{}/workitems/{}/details", e.project, e.workid),
            &json!({"key": "question", "valuetype": "json", "value": question_widget(&question)}),
        )
        .unwrap_or_else(|err| die(format!("could not record the question: {err}")));
    set_state(e, item, "question", None);
    println!("question recorded — this work item parks in `question` when this turn ends and queues again once a human answers. Finish your turn now.");
}

fn reject(e: &Env, reason: &str) {
    let item = calling_item(e);
    let _ = e.api.post(
        &format!("/api/projects/{}/workitems/{}/details", e.project, e.workid),
        &json!({"key": "doc", "valuetype": "text", "value": format!("rejected by the {} agent: {}", e.agent, reason.trim())}),
    );
    set_state(e, item, "parked", Some(&format!("rejected: {}", reason.trim().chars().take(400).collect::<String>())));
    println!("rejected — this work item parks for human review when this turn ends. Finish your turn now.");
}

fn doc(e: &Env, text: String, id: Option<String>) {
    if text.trim().is_empty() {
        die("doc text is empty".into());
    }
    let target = match id {
        Some(n) => {
            let all = items(e);
            resolve_id(e, &all, &n)
        }
        None if !e.workid.is_empty() => e.workid.clone(),
        None => die("no target: pass --id or run inside a work item".into()),
    };
    match e.api.post(
        &format!("/api/projects/{}/workitems/{}/details", e.project, target),
        &json!({"key": "doc", "valuetype": "text", "value": text.trim_end()}),
    ) {
        Ok(row) => println!("doc #{} appended to {target}", row.get("order").and_then(|o| o.as_i64()).unwrap_or(-1)),
        Err(err) => die(format!("doc rejected: {err}")),
    }
}

fn critreview(e: &Env, file: Option<String>, context: Vec<String>, max_retry: u32, disposition: Option<String>, round: Option<i64>) {
    let details_path = format!("/api/projects/{}/workitems/{}/details", e.project, e.workid);
    if let Some(d) = disposition {
        if !["revised", "rejected", "no-findings"].contains(&d.as_str()) {
            die("--disposition must be revised | rejected | no-findings".into());
        }
        let details = e.api.get(&details_path).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let mut reviews: Vec<Value> = details.into_iter().filter(|x| x.get("key").and_then(|k| k.as_str()) == Some("review")).collect();
        reviews.sort_by_key(|x| x.get("order").and_then(|o| o.as_i64()).unwrap_or(0));
        let target = match round {
            Some(r) => reviews.into_iter().find(|x| x.get("value").and_then(|v| v.get("round")).and_then(|n| n.as_i64()) == Some(r)),
            None => reviews.pop(),
        };
        let Some(mut row) = target else { die("no critique round to report on — run `iter critreview --file <material>` first".into()) };
        let order = row.get("order").and_then(|o| o.as_i64()).unwrap_or(0);
        row["value"]["disposition"] = json!(d);
        e.api
            .put(&format!("{details_path}/{order}"), &json!({"key": "review", "valuetype": "json", "value": row["value"]}))
            .unwrap_or_else(|err| die(format!("could not record the disposition: {err}")));
        println!("critreview: round {} disposition = {d}", row["value"]["round"]);
        return;
    }
    let Some(path) = file else { die("--file <material> is required (or --disposition to report on a round)".into()) };
    if e.workid.is_empty() {
        die("ITER_WORKID is not set — critreview records against the calling work item".into());
    }
    let material = std::fs::read_to_string(&path).unwrap_or_else(|err| die(format!("cannot read {path}: {err}")));
    let critic = e.api.get("/api/tooling/_critic").unwrap_or_else(|_| die("no `_critic` tooling row is defined (Agent Tooling in the webui)".into()));
    let persona = critic.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
    let model = critic.get("model").and_then(|m| m.as_str()).unwrap_or("opus").to_string();
    let flags: Vec<String> = critic.get("flags").and_then(|f| f.as_str()).unwrap_or("").split_whitespace().map(String::from).collect();
    let timeout = critic.get("timeoutsec").and_then(|t| t.as_u64()).unwrap_or(1800);
    let mut prompt = format!("{persona}\n\n# Material under review (from {path})\n\n{material}\n");
    if !context.is_empty() {
        prompt.push_str("\n# Context files (read them before judging)\n");
        for c in &context {
            prompt.push_str(&format!("- {c}\n"));
        }
    }
    let cwd = if e.topdir.is_empty() { ".".to_string() } else { e.topdir.clone() };
    let mut last_err = String::new();
    for attempt in 1..=max_retry.max(1) {
        match crate::work::run_critic(&cwd, &prompt, &model, &flags, timeout) {
            Ok(out) if !out.text.trim().is_empty() => {
                let details = e.api.get(&details_path).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let round = details
                    .iter()
                    .filter(|x| x.get("key").and_then(|k| k.as_str()) == Some("review"))
                    .filter_map(|x| x.get("value").and_then(|v| v.get("round")).and_then(|n| n.as_i64()))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let _ = e.api.post(
                    &details_path,
                    &json!({"key": "review", "valuetype": "json", "value": {
                        "round": round, "persona": "_critic", "agent_type": e.agent,
                        "critique": out.text.trim(), "disposition": "",
                        "material": material.chars().take(60_000).collect::<String>(),
                        "created_at": iter_core::now_utc(),
                    }}),
                );
                println!("{}", out.text.trim());
                eprintln!("critreview: recorded as round {round}. When you have acted on it, report back with: iter critreview --disposition <revised|rejected|no-findings> --round {round}");
                return;
            }
            Ok(_) => last_err = "critic returned empty output".into(),
            Err(err) => last_err = err,
        }
        eprintln!("critreview: attempt {attempt} failed ({last_err})");
    }
    die(format!("critical review failed: {last_err}"));
}

fn capability(e: &Env, name: Option<String>) {
    let rows = e.api.get("/api/tooling").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let caps: Vec<&Value> = rows.iter().filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("capability")).collect();
    match name {
        None => {
            for c in caps {
                println!("{}: {}", c.get("name").and_then(|n| n.as_str()).unwrap_or(""), c.get("desc").and_then(|d| d.as_str()).unwrap_or(""));
            }
        }
        Some(n) => {
            let want = n.trim().trim_start_matches('_').trim_end_matches(".md");
            let hit = caps.iter().find(|c| {
                let cn = c.get("name").and_then(|x| x.as_str()).unwrap_or("");
                cn == n || cn.trim_start_matches('_') == want
            });
            match hit {
                Some(c) => println!("{}", c.get("body").and_then(|b| b.as_str()).unwrap_or("")),
                None => die(format!("no capability named '{n}' — run `iter capability` to list them")),
            }
        }
    }
    std::process::exit(0);
}

fn status(e: &Env) {
    let mut all = items(e);
    let order = |s: &str| match s {
        "in-progress" => 0,
        "queued" => 1,
        "question" => 2,
        "paused" => 3,
        "parked" => 4,
        "scheduled" => 5,
        "failed" => 6,
        _ => 7,
    };
    all.retain(|i| !matches!(i.get("state").and_then(|s| s.as_str()), Some("complete") | Some("failed")));
    all.sort_by_key(|i| (order(i.get("state").and_then(|s| s.as_str()).unwrap_or("")), i.get("priority").and_then(|p| p.as_i64()).unwrap_or(5)));
    for i in &all {
        let id = i.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let blocked: Vec<String> = i.get("blockedby").and_then(|b| b.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x[x.len().saturating_sub(12)..].to_string()).collect()).unwrap_or_default();
        println!(
            "{:<11} P{:<2} {:<10} {}  {}{}",
            i.get("state").and_then(|s| s.as_str()).unwrap_or(""),
            i.get("priority").and_then(|p| p.as_i64()).unwrap_or(5),
            i.get("agent").and_then(|a| a.as_str()).unwrap_or(""),
            &id[id.len().saturating_sub(12)..],
            i.get("name").and_then(|n| n.as_str()).unwrap_or(""),
            if blocked.is_empty() { String::new() } else { format!("  blocked-by: {}", blocked.join(",")) }
        );
    }
    println!("{} open work item(s) in {}", all.len(), e.project);
    std::process::exit(0);
}

/// Local-file verbs: hand off to the V2 binary with its own project root.
fn delegate(rest: Vec<String>) {
    let verb = rest.first().cloned().unwrap_or_default();
    let (bin, root) = match (std::env::var("ITER_V2_BIN"), std::env::var("ITER_V2_PROJECT")) {
        (Ok(b), Ok(r)) if !b.is_empty() => (b, r),
        _ => die(format!(
            "`iter {verb}` is a local-file verb that V3 delegates to the V2 binary, and none is configured (ITER_V2_BIN / ITER_V2_PROJECT — the engine sets them when {{topdir}}/devops/iter exists)"
        )),
    };
    let mut args: Vec<String> = vec![verb.clone()];
    let mut has_project = false;
    let mut it = rest.iter().skip(1).peekable();
    while let Some(a) = it.next() {
        if a == "--project" {
            has_project = true;
            args.push(a.clone());
            if let Some(v) = it.next() {
                // V2 wants its own .iter root, whatever the caller passed
                let _ = v;
                args.push(root.clone());
            }
        } else {
            args.push(a.clone());
        }
    }
    if !has_project {
        args.push("--project".into());
        args.push(root.clone());
    }
    let status = std::process::Command::new(&bin).args(&args).status().unwrap_or_else(|err| die(format!("cannot run {bin}: {err}")));
    std::process::exit(status.code().unwrap_or(1));
}
