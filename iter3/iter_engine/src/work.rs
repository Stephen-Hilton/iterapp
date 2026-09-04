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
    for extra in &item.prework {
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
    for extra in &item.postwork {
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
    let promptbody = agent_def.get("promptbody").and_then(|p| p.as_str()).unwrap_or("");
    let overrides = project.agents.get(&item.agent).cloned().unwrap_or(Value::Null);
    let model = overrides
        .get("model")
        .and_then(|m| m.as_str())
        .or_else(|| agent_def.get("model").and_then(|m| m.as_str()))
        .unwrap_or("")
        .to_string();
    let flags = overrides
        .get("flags")
        .and_then(|f| f.as_str())
        .or_else(|| agent_def.get("flags").and_then(|f| f.as_str()))
        .unwrap_or("")
        .to_string();

    let request = request_text(details, item);
    let feedback = gate::feedback_section(details);
    let mut prompt = format!(
        "# Project\n{}\n\n# Agent role\n{}\n\n# Workitem: {}\n{}\n\n{}",
        project.desc, promptbody, item.name, request, gate::WORKER_CLOSE_GATE_PROMPT
    );
    if !feedback.is_empty() {
        prompt.push('\n');
        prompt.push_str(&feedback);
    }

    let extra: Vec<String> = flags.split_whitespace().map(String::from).collect();
    let raw = spawn_claude(project, topdir, account, &prompt, &model, &extra, agent_timeout(project, item))?;
    Ok(parse_claude_json(&raw))
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
