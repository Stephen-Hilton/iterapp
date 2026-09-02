//! Execute one claimed workitem: enforced git prework, mainwork (exec:shell
//! or a headless claude agent), enforced git postwork, response detail,
//! close, release locks. Runs on its own thread.

use crate::client::Api;
use iter_core::{Project, WorkItem, now_utc};
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn execute(api: &Api, engine_name: &str, project: &Project, topdir: &str, item: WorkItem, account: &str) {
    let result = run_all(api, project, topdir, &item, account);
    close(api, engine_name, project, item, result);
}

fn run_all(api: &Api, project: &Project, topdir: &str, item: &WorkItem, account: &str) -> Result<String, String> {
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
        run_shell(topdir, &item.exec_shell, agent_timeout(project, item))?
    } else {
        run_claude(api, project, topdir, item, account)?
    };

    // git postwork is engine-enforced: changes are ALWAYS committed (and
    // pushed when a remote exists)
    if is_repo {
        let short = &item.id[..8.min(item.id.len())];
        let _ = run_shell(topdir, "git add -A", 60);
        let _ = run_shell(topdir, &format!("git commit -m 'iter: {} ({short})'", sanitize(&item.name)), 60);
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

fn run_claude(api: &Api, project: &Project, topdir: &str, item: &WorkItem, account: &str) -> Result<String, String> {
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

    // request text is detail row order 0
    let details = api
        .get(&format!("/api/projects/{}/workitems/{}/details", project.name, item.id))
        .map_err(|e| e.to_string())?;
    let request = details
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|d| d.get("key").and_then(|k| k.as_str()) == Some("request"))
                .and_then(|d| d.get("value").and_then(|v| v.as_str()).map(String::from))
        })
        .unwrap_or_else(|| item.name.clone());

    let prompt = format!(
        "# Project\n{}\n\n# Agent role\n{}\n\n# Workitem: {}\n{}\n",
        project.desc, promptbody, item.name, request
    );

    let mut cmd = Command::new("claude");
    cmd.arg("-p").arg(&prompt).arg("--output-format").arg("text");
    // usage tracking: wire this session's statusline to the collector, teeing
    // server-authoritative rate_limits into THIS account's snapshot file
    cmd.arg("--settings").arg(crate::usage::statusline_settings(account));
    if !model.is_empty() {
        cmd.arg("--model").arg(&model);
    }
    for f in flags.split_whitespace() {
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
    wait_with_timeout(cmd, agent_timeout(project, item))
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

fn close(api: &Api, _engine_name: &str, project: &Project, item: WorkItem, result: Result<String, String>) {
    // response detail row at next order
    let next_order = api
        .get(&format!("/api/projects/{}/workitems/{}/details", project.name, item.id))
        .ok()
        .and_then(|v| v.as_array().map(|a| a.len() as i64))
        .unwrap_or(0)
        .max(1);
    let (ok, text) = match &result {
        Ok(out) => (true, out.clone()),
        Err(e) => (false, e.clone()),
    };
    let _ = api.put(
        &format!("/api/projects/{}/workitems/{}/details/{}", project.name, item.id, next_order),
        &json!({"key": if ok {"response"} else {"error"}, "valuetype": "text", "value": text}),
    );

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
        if ok {
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
        &item.id[..8.min(item.id.len())],
        item.name,
        if ok { "complete" } else { "failed/retry" }
    );
}
