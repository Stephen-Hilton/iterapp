use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::agents::AgentDef;

/// One prompt turn submitted to the agent session.
#[derive(Clone)]
pub struct Turn {
    pub label: String,
    pub prompt: String,
}

/// A live headless Claude Code session. The first turn creates it (`claude -p`);
/// every later turn resumes it (`claude -p --resume <sid>`), so the agent keeps full
/// context across prework → mainwork → postwork.
///
/// Every session gets ITER_BIN (this executable's absolute path) and ITER_PROJECT
/// (the project root owning the queue) in its environment, so agent handoffs —
/// `"$ITER_BIN" add --project "$ITER_PROJECT" …` — work from any codepath without
/// PATH luck or cwd guessing.
pub struct Session {
    pub agent: AgentDef,
    pub cwd: PathBuf,
    pub session_id: String,
    pub bin: String,
    pub envs: Vec<(String, String)>,
}

impl Session {
    pub fn new(agent: AgentDef, cwd: PathBuf, project_root: PathBuf) -> Session {
        let iter_bin = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "iter".to_string());
        let envs = vec![
            ("ITER_BIN".to_string(), iter_bin),
            ("ITER_PROJECT".to_string(), project_root.to_string_lossy().into_owned()),
        ];
        Session { agent, cwd, session_id: String::new(), bin: claude_bin(), envs }
    }

    /// Submit one turn, wait for it to finish (subject to the agent's work timeout),
    /// and return the result text.
    pub fn run(&mut self, turn: &Turn) -> Result<String, String> {
        let mut args: Vec<String> = vec!["-p".into()];
        if !self.session_id.is_empty() {
            args.push("--resume".into());
            args.push(self.session_id.clone());
        }
        args.push(turn.prompt.clone());
        args.push("--output-format".into());
        args.push("json".into());
        args.push("--model".into());
        args.push(self.agent.model.clone());
        for flag in self.agent.model_flags.split_whitespace() {
            args.push(flag.to_string());
        }
        let stdout = run_with_timeout(&self.bin, &args, &self.cwd, self.agent.max_work_timeout_sec, &self.envs)?;
        let (sid, result) = parse_output(&stdout);
        if !sid.is_empty() {
            self.session_id = sid;
        }
        Ok(result)
    }
}

/// The claude binary, overridable for the fake-runner test mode.
pub fn claude_bin() -> String {
    std::env::var("ITER_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// Parse `claude -p --output-format json` stdout: pull `session_id` and `result`.
/// Anything unparseable falls back to the raw stdout as the result.
fn parse_output(stdout: &str) -> (String, String) {
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(v) => {
            let sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let result = v
                .get("result")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| stdout.trim().to_string());
            (sid, result)
        }
        Err(_) => (String::new(), stdout.trim().to_string()),
    }
}

fn run_with_timeout(
    bin: &str,
    args: &[String],
    cwd: &Path,
    timeout_sec: u64,
    envs: &[(String, String)],
) -> Result<String, String> {
    let child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot spawn {}: {}", bin, e))?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(Duration::from_secs(timeout_sec)) {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            } else {
                Err(format!(
                    "exit {}: {}",
                    output.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
            }
        }
        Ok(Err(e)) => Err(format!("wait failed: {}", e)),
        Err(_) => {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            Err(format!("timed out after {}s (killed)", timeout_sec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_output() {
        let (sid, result) = parse_output(r#"{"type":"result","session_id":"abc-123","result":"did the thing"}"#);
        assert_eq!(sid, "abc-123");
        assert_eq!(result, "did the thing");
    }

    #[test]
    fn falls_back_to_raw_stdout() {
        let (sid, result) = parse_output("not json at all");
        assert_eq!(sid, "");
        assert_eq!(result, "not json at all");
    }

    #[test]
    fn timeout_kills_hung_process() {
        let start = std::time::Instant::now();
        let err = run_with_timeout("sleep", &["30".to_string()], Path::new("/tmp"), 1, &[]).unwrap_err();
        assert!(err.contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn session_runs_fake_binary() {
        // Uses /bin/echo as a degenerate fake: output is not JSON, so the raw text
        // becomes the result and no session id is captured.
        let agent = AgentDef { model: "opus".into(), max_work_timeout_sec: 10, ..Default::default() };
        let mut session = Session::new(agent, PathBuf::from("/tmp"), PathBuf::from("/tmp/proj"));
        session.bin = "/bin/echo".into();
        let out = session.run(&Turn { label: "t".into(), prompt: "hello".into() }).unwrap();
        assert!(out.contains("hello"));
        assert!(session.envs.iter().any(|(k, v)| k == "ITER_PROJECT" && v == "/tmp/proj"));
        assert!(session.envs.iter().any(|(k, v)| k == "ITER_BIN" && !v.is_empty()));
    }
}
