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

/// What a completed turn produced, including the CLI's own cost accounting.
pub struct TurnOutcome {
    pub text: String,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
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
    /// True (engine-spawned agents): the claude process leads its own process
    /// group, and a timeout kill takes the whole group — including any
    /// `iter critreview` critic subprocess the agent spawned. False (the critic
    /// itself): stay in the calling agent's group so the engine's kill reaches it.
    pub own_group: bool,
}

impl Session {
    pub fn new(agent: AgentDef, cwd: PathBuf, project_root: PathBuf) -> Session {
        let iter_bin = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "iter".to_string());
        let envs = vec![
            ("ITER_BIN".to_string(), iter_bin),
            ("ITER_PROJECT".to_string(), project_root.to_string_lossy().into_owned()),
            // Let the agent's Bash tool wait on long synchronous calls (critreview)
            // up to the agent's own work timeout — the natural upper bound.
            (
                "BASH_MAX_TIMEOUT_MS".to_string(),
                agent.max_work_timeout_sec.saturating_mul(1000).to_string(),
            ),
        ];
        Session { agent, cwd, session_id: String::new(), bin: claude_bin(), envs, own_group: true }
    }

    /// Submit one turn, wait for it to finish (subject to the agent's work timeout),
    /// and return the result text plus the turn's cost accounting.
    pub fn run(&mut self, turn: &Turn) -> Result<TurnOutcome, String> {
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
        let stdout = run_with_timeout(&self.bin, &args, &self.cwd, self.agent.max_work_timeout_sec, &self.envs, self.own_group)?;
        let (sid, outcome) = parse_output(&stdout);
        if !sid.is_empty() {
            self.session_id = sid;
        }
        Ok(outcome)
    }
}

/// The claude binary, overridable for the fake-runner test mode.
pub fn claude_bin() -> String {
    std::env::var("ITER_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// Parse `claude -p --output-format json` stdout: pull `session_id`, `result`, and
/// the turn's cost accounting (`total_cost_usd`, `usage`). Anything unparseable
/// falls back to the raw stdout as the result with zero cost.
fn parse_output(stdout: &str) -> (String, TurnOutcome) {
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Ok(v) => {
            let sid = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let text = v
                .get("result")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| stdout.trim().to_string());
            let usage = v.get("usage");
            let tok = |key: &str| {
                usage.and_then(|u| u.get(key)).and_then(|t| t.as_u64()).unwrap_or(0)
            };
            (
                sid,
                TurnOutcome {
                    text,
                    cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()).unwrap_or(0.0),
                    input_tokens: tok("input_tokens"),
                    output_tokens: tok("output_tokens"),
                },
            )
        }
        Err(_) => (
            String::new(),
            TurnOutcome { text: stdout.trim().to_string(), cost_usd: 0.0, input_tokens: 0, output_tokens: 0 },
        ),
    }
}

fn run_with_timeout(
    bin: &str,
    args: &[String],
    cwd: &Path,
    timeout_sec: u64,
    envs: &[(String, String)],
    own_group: bool,
) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .envs(envs.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    if own_group {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().map_err(|e| format!("cannot spawn {}: {}", bin, e))?;
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
                // claude -p reports usage-limit errors on stdout and exits 1 with an
                // empty stderr; fall back so spend::is_usage_limit_error can see them.
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let detail = if stderr.is_empty() {
                    String::from_utf8_lossy(&output.stdout).trim().to_string()
                } else {
                    stderr
                };
                Err(format!("exit {}: {}", output.status.code().unwrap_or(-1), detail))
            }
        }
        Ok(Err(e)) => Err(format!("wait failed: {}", e)),
        Err(_) => {
            // Group leaders get a group kill (negative pid) so the whole tree dies —
            // the agent, its shells, and any critic subprocess they spawned.
            if own_group && cfg!(unix) {
                let _ = Command::new("sh").arg("-c").arg(format!("kill -9 -{}", pid)).status();
            } else {
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
            Err(format!("timed out after {}s (killed)", timeout_sec))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_output_with_cost() {
        let (sid, out) = parse_output(
            r#"{"type":"result","session_id":"abc-123","result":"did the thing","total_cost_usd":0.42,"usage":{"input_tokens":100,"output_tokens":50}}"#,
        );
        assert_eq!(sid, "abc-123");
        assert_eq!(out.text, "did the thing");
        assert!((out.cost_usd - 0.42).abs() < 1e-9);
        assert_eq!(out.input_tokens, 100);
        assert_eq!(out.output_tokens, 50);
    }

    #[test]
    fn falls_back_to_raw_stdout() {
        let (sid, out) = parse_output("not json at all");
        assert_eq!(sid, "");
        assert_eq!(out.text, "not json at all");
        assert_eq!(out.cost_usd, 0.0);
    }

    #[test]
    fn nonzero_exit_falls_back_to_stdout_when_stderr_empty() {
        let err = run_with_timeout(
            "sh",
            &["-c".to_string(), "echo 'Claude AI usage limit reached|1765500000'; exit 1".to_string()],
            Path::new("/tmp"),
            10,
            &[],
            false,
        )
        .unwrap_err();
        assert!(err.contains("usage limit reached"), "got: {}", err);
        assert!(crate::spend::is_usage_limit_error(&err));
    }

    #[test]
    fn nonzero_exit_prefers_stderr() {
        let err = run_with_timeout(
            "sh",
            &["-c".to_string(), "echo out; echo 'real error' >&2; exit 1".to_string()],
            Path::new("/tmp"),
            10,
            &[],
            false,
        )
        .unwrap_err();
        assert_eq!(err, "exit 1: real error");
    }

    #[test]
    fn timeout_kills_hung_process() {
        let start = std::time::Instant::now();
        let err = run_with_timeout("sleep", &["30".to_string()], Path::new("/tmp"), 1, &[], true).unwrap_err();
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
        assert!(out.text.contains("hello"));
        assert!(session.envs.iter().any(|(k, v)| k == "ITER_PROJECT" && v == "/tmp/proj"));
        assert!(session.envs.iter().any(|(k, v)| k == "ITER_BIN" && !v.is_empty()));
        assert!(session.envs.iter().any(|(k, v)| k == "BASH_MAX_TIMEOUT_MS" && v == "10000"));
    }
}
