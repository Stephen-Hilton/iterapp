//! Execute one claimed workitem: enforced git prework, mainwork (exec:shell
//! or a headless claude agent), enforced git postwork, response detail,
//! close gate (spec: Close Gate), close, release locks. Runs on its own thread.

use crate::client::Api;
use crate::gate::{self, Evidence, Verdict};
use iter_core::{CloseGate, Project, WorkItem, close_gate_for, now_utc};
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What a run produced.  `subtype` is Claude Code's result subtype
/// ("success", "error_max_turns", ...); exec items and text fallbacks say
/// "success".
#[derive(Debug, Clone, Default)]
pub struct RunOut {
    pub text: String,
    pub subtype: String,
    pub num_turns: u64,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Stop requests the engine tick has seen for items this engine is running;
/// the wait loop kills the session the moment its workid appears.
pub static STOP_REQUESTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
pub const STOPPED_BY_USER: &str = "STOPPED by user mid-run";
thread_local! {
    /// the workid the current worker thread is running (for the wait loop)
    static CURRENT_WORKID: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

impl RunOut {
    fn plain(text: String) -> Self {
        Self { text, subtype: "success".into(), num_turns: 0, cost_usd: 0.0, input_tokens: 0, output_tokens: 0 }
    }
}

/// Everything close() needs to run the gate for an agent item.
struct GateCtx {
    gate: CloseGate,
    request: String,
    details: Vec<Value>,
    head_before: String,
    account: String,
    topdir: String,
}

pub fn execute(api: &Api, engine_name: &str, project: &Project, topdir: &str, item: WorkItem, account: &str) {
    CURRENT_WORKID.with(|w| *w.borrow_mut() = item.id.clone());
    let details = fetch_details(api, project, &item);

    // a human answered the close-gate widget "accept": close without running
    if item.agent != "exec" && gate::accepted_by_human(&details) {
        println!("[engine] {} '{}': close-gate widget answered accept — closing without a run",
            short(&item.id), item.name);
        let out = RunOut::plain("closed complete by a human via the close-gate widget (accept)".into());
        close(api, engine_name, project, item, Ok(out), None);
        return;
    }

    let head_before = git_head(topdir);
    let result = run_all(api, project, topdir, &item, account, &details);
    // `iter ask` / `iter reject` move the item to question/parked mid-run; the
    // close must keep that state (and skip the gate) instead of completing it
    if item.agent != "exec" {
        if let Ok(fresh) = api.get(&format!("/api/projects/{}/workitems/{}", project.name, item.id)) {
            let st = fresh.get("state").and_then(|s| s.as_str()).unwrap_or("");
            if st == "question" || st == "parked" {
                println!("[engine] {} '{}': agent moved it to {} during the run — keeping that", short(&item.id), item.name, st);
                close_keep_state(api, project, item, result, st);
                return;
            }
        }
    }
    let ctx = if item.agent == "exec" {
        None
    } else {
        let (agent_def, overrides) = agent_config(api, project, &item);
        Some(GateCtx {
            gate: close_gate_for(&agent_def, &overrides),
            request: request_text(&details, &item),
            details,
            head_before,
            account: account.to_string(),
            topdir: topdir.to_string(),
        })
    };
    close(api, engine_name, project, item, result, ctx);
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

fn fetch_details(api: &Api, project: &Project, item: &WorkItem) -> Vec<Value> {
    api.get(&format!("/api/projects/{}/workitems/{}/details", project.name, item.id))
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// request text is detail row key "request" (order 0), else the name
fn request_text(details: &[Value], item: &WorkItem) -> String {
    details
        .iter()
        .find(|d| d.get("key").and_then(|k| k.as_str()) == Some("request"))
        .and_then(|d| d.get("value").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_else(|| item.name.clone())
}

fn agent_config(api: &Api, project: &Project, item: &WorkItem) -> (Value, Value) {
    let agent_def = api.get(&format!("/api/agents/{}", item.agent)).unwrap_or(Value::Null);
    let overrides = project.agents.get(&item.agent).cloned().unwrap_or(Value::Null);
    (agent_def, overrides)
}

fn run_all(
    api: &Api,
    project: &Project,
    topdir: &str,
    item: &WorkItem,
    account: &str,
    details: &[Value],
) -> Result<RunOut, String> {
    let is_repo = std::path::Path::new(topdir).join(".git").exists();
    let has_remote = is_repo
        && run_shell(topdir, "git remote", 15).map(|o| !o.trim().is_empty()).unwrap_or(false);

    // git prework is engine-enforced (decided 2026-09-01), not optional
    if is_repo && has_remote {
        run_shell(topdir, "git pull --no-rebase", 120)?;
    }
    // prose steps (agent_tooling kind prepost) run as agent turns inside
    // run_claude; only the rest are engine-run shell steps
    let prose: std::collections::HashSet<String> = api
        .get("/api/tooling")
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|r| r.get("kind").and_then(|k| k.as_str()) == Some("prepost"))
        .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
        .collect();
    for extra in item.prework.iter().filter(|p| !prose.contains(*p)) {
        run_named_ppw(api, project, topdir, extra, item)?;
    }

    let output = if item.agent == "exec" {
        if item.exec_shell.trim().is_empty() {
            return Err("exec item has empty exec_shell".into());
        }
        RunOut::plain(run_shell(topdir, &item.exec_shell, agent_timeout(project, item, &Value::Null))?)
    } else {
        run_claude(api, project, topdir, item, account, details)?
    };

    // git postwork is engine-enforced: changes are ALWAYS committed (and
    // pushed when a remote exists)
    if is_repo {
        let _ = run_shell(topdir, "git add -A", 60);
        let _ = run_shell(topdir, &format!("git commit -m 'iter: {} ({})'", sanitize(&item.name), short(&item.id)), 60);
        if has_remote {
            run_shell(topdir, "git push", 180)?;
        }
    }
    for extra in item.postwork.iter().filter(|p| !prose.contains(*p)) {
        run_named_ppw(api, project, topdir, extra, item)?;
    }
    Ok(output)
}

fn sanitize(s: &str) -> String {
    s.replace('\'', "").chars().take(120).collect()
}

/// Session timeout: the project's per-agent override, else the agent record's
/// `timeoutsec` (the Settings field), else 3600.
fn agent_timeout(project: &Project, item: &WorkItem, agent_def: &Value) -> u64 {
    project
        .agents
        .get(&item.agent)
        .and_then(|o| o.get("timeoutsec"))
        .and_then(|t| t.as_u64())
        .or_else(|| agent_def.get("timeoutsec").and_then(|t| t.as_u64()))
        .filter(|t| *t > 0)
        .unwrap_or(3600)
}

/// Named pre/postwork beyond the enforced git set, from iter3_project_prepostwork.
fn run_named_ppw(
    api: &Api,
    project: &Project,
    topdir: &str,
    name: &str,
    _item: &WorkItem,
) -> Result<(), String> {
    // the enforced git basics are implicit; ignore them if listed explicitly
    if ["git-pull", "git-commit", "git-push"].contains(&name) {
        return Ok(());
    }
    let rows = api
        .get(&format!("/api/projects/{}/prepostwork", project.name))
        .map_err(|e| e.to_string())?;
    let row = rows
        .as_array()
        .and_then(|a| {
            a.iter().find(|r| r.get("name").and_then(|n| n.as_str()) == Some(name)).cloned()
        })
        .ok_or_else(|| format!("prepostwork '{name}' not defined"))?;
    let shell = row.get("shell").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let timeout = row.get("timeoutsec").and_then(|t| t.as_u64()).unwrap_or(30);
    let failhalt = row.get("failhalt").and_then(|f| f.as_bool()).unwrap_or(true);
    match run_shell(topdir, &shell, timeout) {
        Ok(_) => Ok(()),
        Err(e) if failhalt => Err(format!("prepostwork '{name}' failed: {e}")),
        Err(e) => {
            eprintln!("[engine] prepostwork '{name}' failed (failhalt=false): {e}");
            Ok(())
        }
    }
}

fn run_claude(
    api: &Api,
    project: &Project,
    topdir: &str,
    item: &WorkItem,
    account: &str,
    details: &[Value],
) -> Result<RunOut, String> {
    let agent_def = api
        .get(&format!("/api/agents/{}", item.agent))
        .map_err(|e| format!("agent '{}' not defined in iter_data: {e}", item.agent))?;
    let promptbody = agent_def.get("promptbody").and_then(|p| p.as_str()).unwrap_or("").to_string();
    let overrides = project.agents.get(&item.agent).cloned().unwrap_or(Value::Null);
    let model = if !item.model.trim().is_empty() {
        item.model.trim().to_string()
    } else {
        overrides.get("model").and_then(|m| m.as_str())
            .or_else(|| agent_def.get("model").and_then(|m| m.as_str())).unwrap_or("").to_string()
    };
    let flags = overrides.get("flags").and_then(|f| f.as_str())
        .or_else(|| agent_def.get("flags").and_then(|f| f.as_str())).unwrap_or("").to_string();
    let timeout = agent_timeout(project, item, &agent_def);

    // central tooling: shared rules, capability index, source instructions, prose steps
    let tooling_rows = api.get("/api/tooling").ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let tooling = crate::prompt::Tooling::from_rows(&tooling_rows);
    let top = std::path::Path::new(topdir);
    let head = crate::prompt::read_head(project, top);
    let codepath = item
        .lockdirs
        .first()
        .map(|d| crate::prompt::expand_topdir_token(d, top))
        .unwrap_or_else(|| topdir.to_string());
    let codepath = std::path::PathBuf::from(codepath.trim_end_matches('/'));

    // who asked: a workitem id in createdby means an agent handoff — name its type
    let createdby_agent = if !item.createdby.is_empty() && item.createdby.len() >= 32 {
        api.get(&format!("/api/projects/{}/workitems/{}", project.name, item.createdby))
            .ok().and_then(|p| p.get("agent").and_then(|a| a.as_str()).map(String::from)).unwrap_or_default()
    } else {
        String::new()
    };
    let last_response = details
        .iter()
        .filter(|d| d.get("key").and_then(|k| k.as_str()) == Some("response"))
        .max_by_key(|d| d.get("order").and_then(|o| o.as_i64()).unwrap_or(0))
        .and_then(|d| d.get("value").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();

    let (spin, context_files, warnings) = crate::prompt::spinup(&crate::prompt::SpinupInput {
        agent_body: &promptbody,
        tooling: &tooling,
        head: &head,
        project,
        item,
        codepath: &codepath,
        topdir: top,
        requestedby: &item.requestedby,
        createdby_agent: &createdby_agent,
        last_response_tail: &last_response,
        close_gate_paragraph: &format!("\n\n{}", gate::WORKER_CLOSE_GATE_PROMPT),
    });
    for w in &warnings {
        eprintln!("[engine] {} context: {w}", short(&item.id));
    }

    // the request + an answered question (if the item came back from `question`)
    let request = request_text(details, item);
    let answered = crate::prompt::answered_question(details);
    if let Some((order, _, _)) = &answered {
        // mark the answer as shown so a later run does not repeat it
        if let Some(row) = details.iter().find(|d| d.get("order").and_then(|o| o.as_i64()) == Some(*order)) {
            let mut v = row.get("value").cloned().unwrap_or(Value::Null);
            v["surfaced"] = json!(true);
            let _ = api.put(
                &format!("/api/projects/{}/workitems/{}/details/{}", project.name, item.id, order),
                &json!({"key": "question", "valuetype": "json", "value": v}),
            );
        }
    }
    let mut main = crate::prompt::mainwork_prompt(&request, answered.map(|(_, q, a)| (q, a)));
    let feedback = gate::feedback_section(details);
    if !feedback.is_empty() {
        main.push_str("\n\n");
        main.push_str(&feedback);
    }

    // turn sequence: prose prework → mainwork → prose postwork → self-check
    let mut turns: Vec<(String, String)> = Vec::new();
    for step in &item.prework {
        if let Some(body) = tooling.prepost.get(step) {
            turns.push((format!("prework:{step}"), body.clone()));
        }
    }
    turns.push(("mainwork".into(), main));
    for step in &item.postwork {
        if let Some(body) = tooling.prepost.get(step) {
            turns.push((format!("postwork:{step}"), body.clone()));
        }
    }
    turns.push(("selfcheck".into(), crate::prompt::selfcheck_prompt(&promptbody, &tooling.shared)));

    // the agent's environment (V2 names kept so the shared rules still apply verbatim)
    let shim = write_iter_shim(topdir)?;
    let mut envs: Vec<(String, String)> = vec![
        ("ITER_BIN".into(), shim.clone()),
        ("ITER_PROJECT".into(), project.name.clone()),
        ("ITER_WORKID".into(), item.id.clone()),
        ("ITER_AGENT".into(), item.agent.clone()),
        ("ITER_TOPDIR".into(), topdir.to_string()),
        ("ITER_MAINFILE".into(), head.mainfile.to_string_lossy().into_owned()),
        ("ITER_CONTEXT_FILES".into(), head.context_files.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(":")),
        ("ITER_ITEM_CONTEXT_FILES".into(), context_files.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(":")),
        ("ITER_TEST_DIR".into(), "tests".into()),
        ("ITER_INTERFACE_DIR".into(), head.interface_dir.clone()),
        ("ITER_USECASE_DIR".into(), head.usecase_dir.clone()),
        ("ITER_DATA_URL".into(), api.base.clone()),
        ("ITER_ENGINE_TOKEN".into(), api.token.clone()),
        ("BASH_MAX_TIMEOUT_MS".into(), timeout.saturating_mul(1000).to_string()),
    ];
    if let Some(dir) = std::path::Path::new(&shim).parent() {
        let path = std::env::var("PATH").unwrap_or_default();
        envs.push(("PATH".into(), format!("{}:{}", dir.display(), path)));
    }
    let extra: Vec<String> = flags.split_whitespace().map(String::from).collect();
    let mut session = Session { sid: String::new(), cwd: codepath.to_string_lossy().into_owned(), model, extra, envs, timeout, account: account.to_string() };
    if !std::path::Path::new(&session.cwd).is_dir() {
        session.cwd = topdir.to_string();
    }
    let mut last = RunOut::default();
    let (mut usd, mut tin, mut tout, mut nturns) = (0.0f64, 0u64, 0u64, 0u64);
    let total = turns.len();
    for (n, (label, prompt)) in turns.into_iter().enumerate() {
        let text = if n == 0 { format!("{spin}\n\n# Step: {label}\n{prompt}") } else { format!("# Step: {label}\n{prompt}") };
        println!("[engine] {} turn {}/{} {label}", short(&item.id), n + 1, total);
        let out = session.turn(project, &text)?;
        usd += out.cost_usd;
        tin += out.input_tokens;
        tout += out.output_tokens;
        nturns += out.num_turns.max(1);
        // a cut-off turn ends the run: the gate sees the subtype and holds the item
        let cut = out.subtype != "success";
        if label == "mainwork" || cut {
            last = out;
        }
        if cut {
            break;
        }
    }
    last.cost_usd = usd;
    last.input_tokens = tin;
    last.output_tokens = tout;
    last.num_turns = nturns;
    Ok(last)
}

/// The `iter critreview` critic: a fresh session, no account routing beyond
/// the ambient token, bounded by the persona's timeout.
pub fn run_critic(cwd: &str, prompt: &str, model: &str, flags: &[String], timeout_sec: u64) -> Result<RunOut, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("stream-json").arg("--verbose");
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    for f in flags {
        cmd.arg(f);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(parse_claude_stream("", &wait_with_timeout(cmd, timeout_sec)?).1)
}

/// One headless claude session across several turns (`--resume`).
struct Session {
    sid: String,
    cwd: String,
    model: String,
    extra: Vec<String>,
    envs: Vec<(String, String)>,
    timeout: u64,
    account: String,
}

impl Session {
    fn turn(&mut self, project: &Project, prompt: &str) -> Result<RunOut, String> {
        let mut args: Vec<String> = Vec::new();
        if !self.sid.is_empty() {
            args.push("--resume".into());
            args.push(self.sid.clone());
        }
        args.extend(self.extra.iter().cloned());
        let raw = spawn_claude_env(project, &self.cwd, &self.account, prompt, &self.model, &args, self.timeout, &self.envs)?;
        let (sid, out) = parse_claude_stream(&self.account, &raw);
        if !sid.is_empty() {
            self.sid = sid;
        }
        Ok(out)
    }
}

/// `{topdir}/.iter/bin/iter` -> this binary's `cli` subcommand, so agents run
/// plain `iter add …` exactly as the shared rules say.
fn write_iter_shim(topdir: &str) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = std::path::Path::new(topdir).join(".iter").join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let shim = dir.join("iter");
    let body = format!("#!/bin/sh\nexec \"{}\" cli \"$@\"\n", exe.display());
    if std::fs::read_to_string(&shim).ok().as_deref() != Some(body.as_str()) {
        std::fs::write(&shim, body).map_err(|e| format!("write {}: {e}", shim.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755));
    }
    Ok(shim.to_string_lossy().into_owned())
}

/// Parse `--output-format stream-json` output: one JSON object per line.
/// The `rate_limit_event` line is written to `account`'s usage snapshot
/// (spec: Usage%) and the `result` line becomes the RunOut (+ session id for
/// `--resume`).  A lone result object (older CLIs, test doubles) or plain
/// text still parse via `parse_claude_json`.
fn parse_claude_stream(account: &str, raw: &str) -> (String, RunOut) {
    let mut sid = String::new();
    let mut result: Option<RunOut> = None;
    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if crate::usage::record_event(account, &v) {
            continue;
        }
        if v.get("type").and_then(|t| t.as_str()) == Some("result") {
            if let Some(s) = v.get("session_id").and_then(|s| s.as_str()) {
                sid = s.to_string();
            }
            result = Some(parse_claude_json(line));
        }
    }
    if let Some(out) = result {
        return (sid, out);
    }
    // not a stream: a lone result object (possibly after warnings) or plain text
    let trimmed = raw.trim();
    let candidate = trimmed.find('{').map(|i| &trimmed[i..]).unwrap_or("");
    if let Ok(v) = serde_json::from_str::<Value>(candidate) {
        sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
    }
    (sid, parse_claude_json(raw))
}

/// Spawn with an explicit environment (the multi-turn session path).
fn spawn_claude_env(
    project: &Project,
    cwd: &str,
    account: &str,
    prompt: &str,
    model: &str,
    extra_args: &[String],
    timeout_sec: u64,
    envs: &[(String, String)],
) -> Result<String, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("stream-json").arg("--verbose");
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    for f in extra_args {
        cmd.arg(f);
    }
    let token = project
        .accounts
        .iter()
        .filter(|a| account.is_empty() || a.name == account)
        .chain(project.accounts.iter())
        .find_map(|a| std::env::var(&a.token_envar).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()));
    if let Some(tok) = token {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", tok);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    wait_with_timeout(cmd, timeout_sec)
}

/// Spawn one headless claude session (json result output) billed to the
/// chosen account; shared by the worker and the verifier.
fn spawn_claude(
    project: &Project,
    topdir: &str,
    account: &str,
    prompt: &str,
    model: &str,
    extra_args: &[String],
    timeout_sec: u64,
) -> Result<String, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("stream-json").arg("--verbose");
    // usage tracking: stream-json carries a rate_limit_event line per session;
    // parse_claude_stream writes it to THIS account's snapshot file
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    for f in extra_args {
        cmd.arg(f);
    }
    // route billing to the CHOSEN account's token (ladder + exclusion picked
    // it); fall back to the first configured token that is set
    let token = project
        .accounts
        .iter()
        .filter(|a| account.is_empty() || a.name == account)
        .chain(project.accounts.iter())
        .find_map(|a| {
            std::env::var(&a.token_envar).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
        });
    if let Some(tok) = token {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", tok);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.current_dir(topdir).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    wait_with_timeout(cmd, timeout_sec)
}

/// ELI5 (spec: Explain / ELI5): one read-only session of the `explain` agent
/// on a work item, run at once — outside the agent cap, no queue, no locks
/// (it writes nothing in the repo) — whose whole output lands on the item as
/// an "explained" detail row.  `agent_def` is the project's `explain` agent
/// record when one exists (model / timeout / body); its flags are ignored:
/// the tool set here is fixed to Read, Glob, Grep.
pub fn explain(api: &Api, project: &Project, topdir: &str, item: &WorkItem, account: &str, agent_def: &Value) {
    let started = Instant::now();
    let details = fetch_details(api, project, item);
    let top = std::path::Path::new(topdir);
    let head = crate::prompt::read_head(project, top);
    let codepath = item
        .lockdirs
        .first()
        .map(|d| crate::prompt::expand_topdir_token(d, top))
        .unwrap_or_else(|| topdir.to_string());
    let codepath = std::path::PathBuf::from(codepath.trim_end_matches('/'));
    let body = agent_def.get("promptbody").and_then(|b| b.as_str()).unwrap_or("");
    // a stub body (the header line alone) means "use the built-in persona"
    let body = if body.trim().lines().count() > 3 { body.to_string() } else { crate::prompt::EXPLAIN_DEFAULT_BODY.to_string() };
    let prompt = crate::prompt::explain_prompt(&crate::prompt::ExplainInput {
        agent_body: &body, head: &head, project, item, details: &details, codepath: &codepath, topdir: top,
    });
    let model = agent_def.get("model").and_then(|m| m.as_str()).unwrap_or("sonnet").trim().to_string();
    let timeout = agent_def.get("timeoutsec").and_then(|t| t.as_u64()).unwrap_or(900).clamp(60, 3600);
    let extra = vec![
        "--allowedTools".to_string(),
        "Read,Glob,Grep".to_string(),
        "--disallowedTools".to_string(),
        "Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch,Agent".to_string(),
        "--max-turns".to_string(),
        "40".to_string(),
    ];
    let details_path = format!("/api/projects/{}/workitems/{}/details", project.name, item.id);
    let result = spawn_claude(project, topdir, account, &prompt, &model, &extra, timeout)
        .map(|raw| parse_claude_stream(account, &raw).1);
    let secs = started.elapsed().as_secs();
    let value = match &result {
        Ok(out) if out.subtype == "success" && !out.text.trim().is_empty() => out.text.trim().to_string(),
        Ok(out) => format!("Could not explain this item: the session ended with '{}' after {secs}s.{}",
            out.subtype, if out.text.trim().is_empty() { String::new() } else { format!("\n\n{}", out.text.trim()) }),
        Err(e) => format!("Could not explain this item: {}", e.chars().take(800).collect::<String>()),
    };
    if let Err(e) = api.post(&details_path, &json!({"key": "explained", "valuetype": "text", "value": value})) {
        eprintln!("[engine] could not append the explanation to {}: {e}", short(&item.id));
    }
    if let Ok(out) = &result {
        if out.cost_usd > 0.0 || out.input_tokens > 0 {
            let _ = api.post(&details_path, &json!({"key": "spend", "valuetype": "json", "value": {"usd": out.cost_usd,
                "input_tokens": out.input_tokens, "output_tokens": out.output_tokens, "turns": out.num_turns, "agent": "explain"}}));
            let _ = api.post(&format!("/api/projects/{}/spend", project.name),
                &json!({"usd": out.cost_usd, "input_tokens": out.input_tokens, "output_tokens": out.output_tokens, "workid": item.id}));
        }
    }
    if let Err(e) = api.delete(&format!("/api/projects/{}/workitems/{}/explain", project.name, item.id)) {
        eprintln!("[engine] could not clear explain_requested on {}: {e}", short(&item.id));
    }
    println!("[engine] explained {} '{}' in {secs}s ({})", short(&item.id), item.name,
        match &result { Ok(o) => format!("{}, ${:.3}", o.subtype, o.cost_usd), Err(_) => "failed".into() });
}

/// Connectivity nudge (spec: engine chip "test"): `claude -p "."` on haiku
/// with no other context, billed to `account`'s token when one is configured.
/// Proves the CLI + token work; its rate_limit_event line refreshes the
/// account's usage snapshot as a side effect.
pub fn nudge(token: Option<String>, account: &str, cwd: &str) -> Result<(RunOut, u128), String> {
    let started = Instant::now();
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(".").arg("--output-format").arg("stream-json").arg("--verbose").arg("--model").arg("haiku").arg("--max-turns").arg("1");
    if let Some(tok) = token {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", tok);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let raw = wait_with_timeout(cmd, 120)?;
    Ok((parse_claude_stream(account, &raw).1, started.elapsed().as_millis()))
}

/// The result object — the last line of stream-json, or all of
/// `--output-format json`: {"type":"result",
/// "subtype":"success"|"error_max_turns"|..., "result":"<final text>",
/// "num_turns":N, ...}.  Anything that is not that object is treated as
/// plain text output (older CLIs, test doubles).
fn parse_claude_json(raw: &str) -> RunOut {
    let trimmed = raw.trim();
    let candidate = trimmed
        .find('{')
        .map(|i| &trimmed[i..])
        .unwrap_or("");
    if let Ok(v) = serde_json::from_str::<Value>(candidate) {
        if v.get("type").and_then(|t| t.as_str()) == Some("result") || v.get("result").is_some() {
            let text = match v.get("result") {
                Some(Value::String(s)) => s.clone(),
                Some(other) if !other.is_null() => other.to_string(),
                _ => String::new(),
            };
            return RunOut {
                text,
                subtype: v.get("subtype").and_then(|s| s.as_str()).unwrap_or("success").to_string(),
                num_turns: v.get("num_turns").and_then(|n| n.as_u64()).unwrap_or(0),
                cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()).unwrap_or(0.0),
                input_tokens: v.get("usage").and_then(|u| u.get("input_tokens")).and_then(|n| n.as_u64()).unwrap_or(0),
                output_tokens: v.get("usage").and_then(|u| u.get("output_tokens")).and_then(|n| n.as_u64()).unwrap_or(0),
            };
        }
    }
    RunOut::plain(raw.to_string())
}

fn run_shell(cwd: &str, script: &str, timeout_sec: u64) -> Result<String, String> {
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    wait_with_timeout(cmd, timeout_sec)
}

fn git_head(topdir: &str) -> String {
    if !std::path::Path::new(topdir).join(".git").exists() {
        return String::new();
    }
    run_shell(topdir, "git rev-parse HEAD", 15).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn wait_with_timeout(mut cmd: Command, timeout_sec: u64) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // so a stop can take the whole tree down
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    // drain both pipes on their own threads: stream-json echoes every message,
    // and a full 64K pipe would block the child forever if read only at exit
    let slurp = |pipe: Option<Box<dyn std::io::Read + Send>>| -> Option<std::thread::JoinHandle<String>> {
        pipe.map(|mut s| std::thread::spawn(move || { let mut b = String::new(); let _ = s.read_to_string(&mut b); b }))
    };
    let mut out_h = slurp(child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));
    let mut err_h = slurp(child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    let workid = CURRENT_WORKID.with(|w| w.borrow().clone());
    loop {
        if !workid.is_empty() && STOP_REQUESTED.lock().map(|v| v.contains(&workid)).unwrap_or(false) {
            let pid = child.id();
            #[cfg(unix)]
            {
                let _ = Command::new("kill").args(["-TERM", "--", &format!("-{pid}")]).status();
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(STOPPED_BY_USER.into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = out_h.take().and_then(|h| h.join().ok()).unwrap_or_default();
                let err = err_h.take().and_then(|h| h.join().ok()).unwrap_or_default();
                if status.success() {
                    return Ok(out);
                }
                return Err(format!("exit {:?}: {}{}", status.code(), out, err));
            }
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    return Err(format!("timed out after {timeout_sec}s"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("wait failed: {e}")),
        }
    }
}

/// The gate's decision for a successful agent run.
enum GateOutcome {
    Pass,
    /// held back: source ("deterministic" | "verifier"), open list, reason;
    /// `to_human` forces the question state regardless of bounce budget
    Hold { source: &'static str, open: Vec<String>, reason: String, to_human: bool },
}

/// Run the close gate: deterministic checks first (free); the verifier only
/// when they all pass and a verify model is configured.
fn run_gate(api: &Api, project: &Project, item: &WorkItem, out: &RunOut, ctx: &GateCtx) -> (GateOutcome, Evidence) {
    let head_after = git_head(&ctx.topdir);
    let diffstat = if !head_after.is_empty() && head_after != ctx.head_before && !ctx.head_before.is_empty() {
        run_shell(&ctx.topdir, &format!("git diff --stat {} {}", ctx.head_before, head_after), 30)
            .map(|s| gate::clip(s.trim(), 4_000))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let children = if ctx.gate.requires_children {
        api.get(&format!("/api/projects/{}/workitems", project.name))
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|a| a.iter().filter(|w| w.get("createdby").and_then(|c| c.as_str()) == Some(item.id.as_str())).count())
            .unwrap_or(0)
    } else {
        0
    };
    let ev = Evidence {
        result_subtype: out.subtype.clone(),
        num_turns: out.num_turns,
        head_before: ctx.head_before.clone(),
        head_after,
        diffstat,
        children,
        open_reviews: gate::open_reviews(&ctx.details),
    };

    let mut open: Vec<String> = Vec::new();
    if out.subtype != "success" {
        open.push(format!("the agent session ended with '{}' (cut off, not finished)", out.subtype));
    }
    if ev.open_reviews > 0 {
        open.push(format!("{} review row(s) recorded without a disposition", ev.open_reviews));
    }
    if ctx.gate.requires_children && ev.children == 0 {
        open.push("no workitems were created by this item (closegate.requires_children)".into());
    }
    // `iter runtests --fixed` is the TDD completion gate: the LAST fixed-claim
    // must have been upheld (a later upheld claim clears an earlier false one)
    if let Some(c) = gate::last_fixed_claim(&ctx.details) {
        if !c.get("upheld").and_then(|b| b.as_bool()).unwrap_or(false) {
            open.push(format!(
                "the last `iter runtests --fixed` claim was FALSE: testgroup \"{}\" is {} ({})",
                c.get("group").and_then(|g| g.as_str()).unwrap_or("?"),
                c.get("outcome").and_then(|o| o.as_str()).unwrap_or("?"),
                c.get("counts").and_then(|o| o.as_str()).unwrap_or("?")
            ));
        }
    }
    if ctx.gate.requires_commit && !ev.committed() {
        open.push("no new git commit was produced (closegate.requires_commit)".into());
    }
    if !open.is_empty() {
        let reason = format!("deterministic close-gate check(s) failed: {}", open.join("; "));
        return (GateOutcome::Hold { source: "deterministic", open, reason, to_human: false }, ev);
    }

    if ctx.gate.verify.trim().is_empty() {
        return (GateOutcome::Pass, ev);
    }
    let prompt = gate::verifier_prompt(&item.name, &ctx.request, &out.text, &ev);
    let extra = vec![
        "--allowedTools".to_string(),
        "Read,Glob,Grep".to_string(),
        "--max-turns".to_string(),
        ctx.gate.verify_max_turns.max(1).to_string(),
    ];
    let verdict = match spawn_claude(project, &ctx.topdir, &ctx.account, &prompt, ctx.gate.verify.trim(), &extra, 600) {
        Ok(raw) => gate::parse_verdict(&parse_claude_stream(&ctx.account, &raw).1.text),
        Err(e) => Verdict::Unclear { reason: format!("verifier session failed: {}", gate::clip(&e, 500)) },
    };
    match verdict {
        Verdict::Complete => (GateOutcome::Pass, ev),
        Verdict::Incomplete { open, reason } => (
            GateOutcome::Hold { source: "verifier", open, reason: format!("verifier: {reason}"), to_human: false },
            ev,
        ),
        Verdict::Unclear { reason } => (
            GateOutcome::Hold { source: "verifier", open: vec![], reason: format!("verifier unclear: {reason}"), to_human: true },
            ev,
        ),
    }
}

/// Close-out when the agent itself moved the item (question via `iter ask`,
/// parked via `iter reject`): record the response, keep the state, free locks.
fn close_keep_state(api: &Api, project: &Project, item: WorkItem, result: Result<RunOut, String>, state: &str) {
    let details_path = format!("/api/projects/{}/workitems/{}/details", project.name, item.id);
    let (key, text) = match &result {
        Ok(out) => ("response", out.text.clone()),
        Err(e) => ("error", e.clone()),
    };
    let _ = api.post(&details_path, &json!({"key": key, "valuetype": "text", "value": text}));
    for d in &item.lockdirs {
        let _ = api.post(&format!("/api/projects/{}/locks/release", project.name), &json!({"path": d, "workid": item.id}));
    }
    println!("[engine] done {} '{}' -> {} (set by the agent)", short(&item.id), item.name, state);
}

fn close(api: &Api, _engine_name: &str, project: &Project, item: WorkItem, result: Result<RunOut, String>, ctx: Option<GateCtx>) {
    // detail rows are APPENDED (iter_data allocates the order atomically)
    let details_path = format!("/api/projects/{}/workitems/{}/details", project.name, item.id);
    let put_detail = |key: &str, valuetype: &str, value: Value| {
        if let Err(e) = api.post(&details_path, &json!({"key": key, "valuetype": valuetype, "value": value})) {
            eprintln!("[engine] could not append '{key}' detail to {}: {e}", item.id);
        }
    };

    let (ok, text) = match &result {
        Ok(out) => (true, out.text.clone()),
        Err(e) => (false, e.clone()),
    };
    put_detail(if ok { "response" } else { "error" }, "text", json!(text));

    // cost accounting: a "spend" row on the item + the project's daily total
    if let Ok(out) = &result {
        if out.cost_usd > 0.0 || out.input_tokens > 0 {
            put_detail("spend", "json", json!({"usd": out.cost_usd, "input_tokens": out.input_tokens, "output_tokens": out.output_tokens,
                "turns": out.num_turns, "agent": item.agent, "attempt": item.attempt}));
            let _ = api.post(&format!("/api/projects/{}/spend", project.name),
                &json!({"usd": out.cost_usd, "input_tokens": out.input_tokens, "output_tokens": out.output_tokens, "workid": item.id}));
        }
    }
    let stopped = matches!(&result, Err(e) if e == STOPPED_BY_USER);
    if stopped {
        if let Ok(mut v) = STOP_REQUESTED.lock() {
            v.retain(|w| w != &item.id);
        }
    }

    // the close gate decides what "ok" closes to
    let mut gate_hold: Option<(String, bool)> = None; // (short reason, to_question)
    if let (Ok(out), Some(ctx)) = (&result, &ctx) {
        let (outcome, ev) = run_gate(api, project, &item, out, ctx);
        if let GateOutcome::Hold { source, open, reason, to_human } = outcome {
            let bounce = item.gate_bounces + 1;
            let to_question = to_human || item.gate_bounces >= ctx.gate.max_bounces;
            put_detail("verify", "json", gate::verify_row(bounce, source, if to_human { "unclear" } else { "incomplete" }, &open, &reason, &ev));
            if to_question {
                put_detail("question", "json", gate::question_widget(&item.name, bounce, &reason, &open, &out.text));
            }
            println!(
                "[engine] close gate held {} '{}' (bounce {}, {}): {}",
                short(&item.id), item.name, bounce,
                if to_question { "-> question" } else { "-> queued" },
                gate::clip(&reason, 200)
            );
            gate_hold = Some((gate::clip(&reason, 500), to_question));
        }
    }

    // close with a versioned write; on conflict re-read and retry once
    for attempt in 0..2 {
        let fresh = if attempt == 0 {
            serde_json::to_value(&item).unwrap()
        } else {
            match api.get(&format!("/api/projects/{}/workitems/{}", project.name, item.id)) {
                Ok(v) => v,
                Err(_) => break,
            }
        };
        let version = fresh.get("version").and_then(|v| v.as_u64()).unwrap_or(item.version);
        let mut updated = fresh.clone();
        if stopped {
            // workitem_stop.md: parked for human review, never retried
            updated["state"] = json!("parked");
            updated["stop_requested"] = json!(false);
            updated["lasterror"] = json!(STOPPED_BY_USER);
        } else if let Some((reason, to_question)) = &gate_hold {
            updated["state"] = json!(if *to_question { "question" } else { "queued" });
            updated["gate_bounces"] = json!(item.gate_bounces + 1);
            updated["lasterror"] = json!(format!("close gate: {reason}"));
        } else if ok {
            updated["state"] = json!("complete");
            updated["ts"]["complete"] = json!(now_utc());
            updated["lasterror"] = json!("");
        } else {
            let maxattempts = project.failure.maxattempts.max(1);
            updated["lasterror"] = json!(text.chars().take(2000).collect::<String>());
            if item.attempt >= maxattempts {
                updated["state"] = json!("failed");
                updated["ts"]["complete"] = json!(now_utc());
            } else {
                // retry after the project's backoff (V2 retry_backoff_sec)
                let delay = iter_core::retry_delay_sec(&project.failure, item.attempt);
                let until = chrono::Utc::now() + chrono::Duration::seconds(delay as i64);
                updated["state"] = json!("queued");
                updated["retry_after"] = json!(until.format("%Y-%m-%dT%H:%M:%SZ").to_string());
                println!("[engine] {} '{}': attempt {} failed, retry after {}s", short(&item.id), item.name, item.attempt, delay);
            }
        }
        match api.put(
            &format!(
                "/api/projects/{}/workitems/{}?expect_version={}",
                project.name, item.id, version
            ),
            &updated,
        ) {
            Ok(_) => break,
            Err(e) if e.status == 409 && attempt == 0 => continue,
            Err(e) => {
                eprintln!("[engine] close failed for {}: {e}", item.id);
                break;
            }
        }
    }

    // release every lock this workitem held
    for d in &item.lockdirs {
        let _ = api.post(
            &format!("/api/projects/{}/locks/release", project.name),
            &json!({"path": d, "workid": item.id}),
        );
    }
    println!(
        "[engine] done {} '{}' -> {}",
        short(&item.id),
        item.name,
        match (&gate_hold, ok) {
            _ if stopped => "parked (stopped by user)",
            (Some((_, true)), _) => "question (close gate)",
            (Some((_, false)), _) => "queued (close gate bounce)",
            (None, true) => "complete",
            (None, false) => "failed/retry",
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_json_result_is_parsed_and_text_falls_back() {
        let out = parse_claude_json(r#"{"type":"result","subtype":"error_max_turns","num_turns":40,"result":"partial"}"#);
        assert_eq!((out.text.as_str(), out.subtype.as_str(), out.num_turns), ("partial", "error_max_turns", 40));
        let out = parse_claude_json("plain words from an older cli");
        assert_eq!(out.subtype, "success");
        assert_eq!(out.text, "plain words from an older cli");
        // leading noise before the object is tolerated
        let out = parse_claude_json("warn: x\n{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}");
        assert_eq!(out.text, "ok");
    }

    #[test]
    fn stream_json_yields_result_sid_and_writes_the_usage_snapshot() {
        let dir = std::env::temp_dir().join(format!("iter3-usage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: test-only; nothing else in this process reads ITER_USAGE_DIR concurrently
        unsafe { std::env::set_var("ITER_USAGE_DIR", &dir) };
        let raw = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sid-1\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
            "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\",\"isUsingOverage\":false,",
            "\"unifiedWindows\":{\"five_hour\":{\"utilization\":0.25,\"resetsAt\":99999999999},",
            "\"seven_day\":{\"utilization\":0.5,\"resetsAt\":99999999999}}}}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"sid-1\",\"num_turns\":2,",
            "\"total_cost_usd\":0.01,\"usage\":{\"input_tokens\":10,\"output_tokens\":3},\"result\":\"done\"}\n"
        );
        let (sid, out) = parse_claude_stream("Acct", raw);
        assert_eq!(sid, "sid-1");
        assert_eq!((out.text.as_str(), out.subtype.as_str(), out.num_turns, out.input_tokens), ("done", "success", 2, 10));
        let u = crate::usage::read_usage("Acct").expect("snapshot written from the stream");
        assert!((u.five_hour_pct - 25.0).abs() < 1e-9 && (u.seven_day_pct - 50.0).abs() < 1e-9);
        assert_eq!(u.source, "stream");
        // a lone result object (fake claude / older cli) still parses
        let (sid, out) = parse_claude_stream("Acct", "{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s2\",\"result\":\"x\"}");
        assert_eq!((sid.as_str(), out.text.as_str()), ("s2", "x"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
