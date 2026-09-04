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
}

impl RunOut {
    fn plain(text: String) -> Self {
        Self { text, subtype: "success".into(), num_turns: 0 }
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
        RunOut::plain(run_shell(topdir, &item.exec_shell, agent_timeout(project, item))?)
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

fn agent_timeout(project: &Project, item: &WorkItem) -> u64 {
    project
        .agents
        .get(&item.agent)
        .and_then(|o| o.get("timeoutsec"))
        .and_then(|t| t.as_u64())
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
    let timeout = agent_timeout(project, item);

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
    // V2 delegation for local-file verbs (runtests/validate/...): the V2 binary + its project root
    if let Some((bin, root)) = v2_delegate(topdir) {
        envs.push(("ITER_V2_BIN".into(), bin));
        envs.push(("ITER_V2_PROJECT".into(), root));
    }

    let extra: Vec<String> = flags.split_whitespace().map(String::from).collect();
    let mut session = Session { sid: String::new(), cwd: codepath.to_string_lossy().into_owned(), model, extra, envs, timeout, account: account.to_string() };
    if !std::path::Path::new(&session.cwd).is_dir() {
        session.cwd = topdir.to_string();
    }
    let mut last = RunOut::default();
    let total = turns.len();
    for (n, (label, prompt)) in turns.into_iter().enumerate() {
        let text = if n == 0 { format!("{spin}\n\n# Step: {label}\n{prompt}") } else { format!("# Step: {label}\n{prompt}") };
        println!("[engine] {} turn {}/{} {label}", short(&item.id), n + 1, total);
        let out = session.turn(project, &text)?;
        // a cut-off turn ends the run: the gate sees the subtype and holds the item
        let cut = out.subtype != "success";
        if label == "mainwork" || cut {
            last = out;
        }
        if cut {
            break;
        }
    }
    Ok(last)
}

/// The `iter critreview` critic: a fresh session, no account routing beyond
/// the ambient token, bounded by the persona's timeout.
pub fn run_critic(cwd: &str, prompt: &str, model: &str, flags: &[String], timeout_sec: u64) -> Result<RunOut, String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("json");
    if !model.is_empty() {
        cmd.arg("--model").arg(model);
    }
    for f in flags {
        cmd.arg(f);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(parse_claude_json(&wait_with_timeout(cmd, timeout_sec)?))
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
        let (sid, out) = parse_claude_json_sid(&raw);
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

/// The V2 binary + project root when the repo still carries them
/// (`{topdir}/devops/iter` + `{topdir}/devops`), for the local-file verbs V3
/// has not re-implemented (runtests, validate, markers, teststate, usecase…).
fn v2_delegate(topdir: &str) -> Option<(String, String)> {
    if let Ok(b) = std::env::var("ITER_V2_BIN") {
        let root = std::env::var("ITER_V2_PROJECT").unwrap_or_else(|_| topdir.to_string());
        return Some((b, root));
    }
    let cand = std::path::Path::new(topdir).join("devops").join("iter");
    if cand.is_file() {
        return Some((cand.to_string_lossy().into_owned(), cand.parent().unwrap().to_string_lossy().into_owned()));
    }
    None
}

/// Like `parse_claude_json` but also returns the session id for `--resume`.
fn parse_claude_json_sid(raw: &str) -> (String, RunOut) {
    let trimmed = raw.trim();
    let candidate = trimmed.find('{').map(|i| &trimmed[i..]).unwrap_or("");
    if let Ok(v) = serde_json::from_str::<Value>(candidate) {
        let sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        return (sid, parse_claude_json(raw));
    }
    (String::new(), parse_claude_json(raw))
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
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("json");
    cmd.arg("--settings").arg(crate::usage::statusline_settings(account));
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
    cmd.arg("-p").arg(prompt).arg("--output-format").arg("json");
    // usage tracking: wire this session's statusline to the collector, teeing
    // server-authoritative rate_limits into THIS account's snapshot file
    cmd.arg("--settings").arg(crate::usage::statusline_settings(account));
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

/// Connectivity nudge (spec: engine chip "test"): `claude -p "."` on haiku
/// with no other context, billed to `account`'s token when one is configured.
/// Cheap, and its statusline callback refreshes the usage snapshot as a side
/// effect — which is the real payload.
pub fn nudge(token: Option<String>, account: &str, cwd: &str) -> Result<(RunOut, u128), String> {
    let started = Instant::now();
    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(".").arg("--output-format").arg("json").arg("--model").arg("haiku").arg("--max-turns").arg("1");
    cmd.arg("--settings").arg(crate::usage::statusline_settings(account));
    if let Some(tok) = token {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", tok);
    }
    cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let raw = wait_with_timeout(cmd, 120)?;
    Ok((parse_claude_json(&raw), started.elapsed().as_millis()))
}

/// `--output-format json` prints one result object: {"type":"result",
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
    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                let mut err = String::new();
                use std::io::Read;
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
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
        Ok(raw) => gate::parse_verdict(&parse_claude_json(&raw).text),
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
        if let Some((reason, to_question)) = &gate_hold {
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
                updated["state"] = json!("queued"); // retry
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
}
