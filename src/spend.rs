use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-turn spend ledger: every agent turn's `total_cost_usd` and token counts from
/// the claude CLI's JSON output, appended to .iter/.engine/spend.jsonl. This is the
/// same accounting Claude Code itself reports — the engine just keeps the receipts,
/// so a daily budget can auto-drain the loop before credits run dry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SpendEntry {
    pub ts: String,
    pub workid: String,
    pub agent: String,
    pub turn: String,
    pub usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

fn ledger_path(project_root: &Path) -> std::path::PathBuf {
    crate::config::engine_dir(project_root).join("spend.jsonl")
}

pub fn record(project_root: &Path, entry: &SpendEntry) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path(project_root))
    {
        if let Ok(line) = serde_json::to_string(entry) {
            let _ = writeln!(f, "{}", line);
        }
    }
}

/// Total USD spent today (UTC), summed from the ledger.
pub fn today_usd(project_root: &Path) -> f64 {
    let prefix = chrono::Utc::now().format("%Y-%m-%d").to_string();
    sum_for_day(project_root, &prefix)
}

pub fn sum_for_day(project_root: &Path, day_prefix: &str) -> f64 {
    let Ok(text) = std::fs::read_to_string(ledger_path(project_root)) else { return 0.0 };
    text.lines()
        .filter_map(|l| serde_json::from_str::<SpendEntry>(l).ok())
        .filter(|e| e.ts.starts_with(day_prefix))
        .map(|e| e.usd)
        .sum()
}

/// Does an agent-turn error indicate the account hit a usage/credit limit?
/// These are terminal for the whole run (every subsequent turn will fail the same
/// way), so the engine drains instead of burning attempts across the queue.
///
/// Matching is a substring scan over the error text normalized to lowercase with
/// `_`/`-` folded to spaces, so API error codes ("usage_limit_reached") and prose
/// ("Usage limit reached") hit the same needle. Transient throttling ("rate limit",
/// "too many requests", "overloaded") is deliberately NOT matched — it clears on
/// its own and should burn a normal retry, not drain the loop — and "rate limit"
/// is stripped before scanning so "rate limit reached" can't hit the generic
/// "limit reached" needle.
fn normalize_error(error: &str) -> String {
    let lowered = error.to_lowercase().replace(['_', '-'], " ");
    let stripped = lowered.replace("rate limit", " ").replace("ratelimit", " ");
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Time-window usage limits (5h / weekly / …): recoverable — the window resets on a
/// schedule, so the engine holds and auto-resumes rather than draining and exiting.
const WINDOW_NEEDLES: &[&str] = &[
    "usage limit",     // "Claude AI usage limit reached|<epoch>", "usage_limit_reached"
    "usage cap",
    "hit your limit",  // "You've hit your limit · resets 3am"
    "reached your limit",
    "exceeded your limit",
    "limit reached",   // "5-hour limit reached", "weekly limit reached"
    "limit will reset",
    "5 hour limit",
    "weekly limit",
    "monthly limit",
    "daily limit",
    "session limit",
    "plan limit",
    "out of extra usage",
];

/// Credit/billing exhaustion and disabled accounts: nothing resets on its own, so
/// retrying on an interval would fail forever — the engine drains instead.
const TERMINAL_NEEDLES: &[&str] = &[
    "credit balance",  // "Your credit balance is too low"
    "credit limit",
    "out of credit",
    "insufficient credit",
    "insufficient quota",
    "insufficient funds",
    "not enough credit",
    "no credits",
    "credits exhausted",
    "quota exceeded",
    "exceeded your quota",
    "quota exhausted",
    "spending limit",
    "spend limit",
    "spending cap",
    "billing hard limit",
    "payment required", // HTTP 402
    "organization has been disabled",
    "account has been disabled",
    "account is disabled",
];

pub fn is_usage_limit_error(error: &str) -> bool {
    let e = normalize_error(error);
    WINDOW_NEEDLES.iter().chain(TERMINAL_NEEDLES).any(|needle| e.contains(needle))
}

/// The terminal subset: account/billing states no waiting will fix.
pub fn is_account_terminal_error(error: &str) -> bool {
    let e = normalize_error(error);
    TERMINAL_NEEDLES.iter().any(|needle| e.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-spend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter/.engine")).unwrap();
        dir
    }

    #[test]
    fn ledger_records_and_sums_by_day() {
        let root = tmpdir();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        record(&root, &SpendEntry { ts: format!("{}T10:00:00Z", today), usd: 1.25, ..Default::default() });
        record(&root, &SpendEntry { ts: format!("{}T11:00:00Z", today), usd: 0.75, ..Default::default() });
        record(&root, &SpendEntry { ts: "2020-01-01T00:00:00Z".into(), usd: 99.0, ..Default::default() });
        assert!((today_usd(&root) - 2.0).abs() < 1e-9, "old days must not count");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn usage_limit_detection() {
        assert!(is_usage_limit_error("exit 1: Claude AI usage limit reached|1765500000"));
        assert!(is_usage_limit_error("Your credit balance is too low"));
        assert!(is_usage_limit_error("You've hit your limit · resets 3am"));
        assert!(!is_usage_limit_error("exit 1: connection refused"));
        assert!(!is_usage_limit_error("timed out after 3600s (killed)"));
    }

    #[test]
    fn usage_limit_detection_hardened() {
        // API error codes: underscores fold to spaces.
        assert!(is_usage_limit_error(r#"{"type":"error","error":{"type":"usage_limit_reached"}}"#));
        // Newer CLI / product phrasings.
        assert!(is_usage_limit_error("You've reached your limit for today"));
        assert!(is_usage_limit_error("Your limit will reset at 10pm (America/Los_Angeles)"));
        assert!(is_usage_limit_error("5-hour limit reached ∙ resets 6pm"));
        assert!(is_usage_limit_error("You've exceeded your weekly limit"));
        assert!(is_usage_limit_error("Out of extra usage — buy more or wait for reset"));
        // Credit / billing exhaustion.
        assert!(is_usage_limit_error("Insufficient credits to complete this request"));
        assert!(is_usage_limit_error("exit 1: 402 Payment Required"));
        assert!(is_usage_limit_error("quota_exceeded: monthly spend cap"));
        assert!(is_usage_limit_error("You have no credits remaining"));
        // Terminal account states.
        assert!(is_usage_limit_error("This organization has been disabled."));
        // Transient throttling must NOT drain the engine.
        assert!(!is_usage_limit_error("exit 1: 429 rate_limit_error: too many requests"));
        assert!(!is_usage_limit_error("Rate limit reached, retrying in 20s"));
        assert!(!is_usage_limit_error("exit 1: overloaded_error"));
        // Per-request errors are not account limits.
        assert!(!is_usage_limit_error("prompt is too long: 210000 tokens > 200000 maximum"));
    }

    #[test]
    fn window_vs_terminal_classification() {
        // Window limits: recoverable, must NOT classify as terminal.
        assert!(!is_account_terminal_error("Claude AI usage limit reached|1765500000"));
        assert!(!is_account_terminal_error("5-hour limit reached ∙ resets 6pm"));
        assert!(!is_account_terminal_error("You've exceeded your weekly limit"));
        // Billing/account states: terminal.
        assert!(is_account_terminal_error("Your credit balance is too low"));
        assert!(is_account_terminal_error("exit 1: 402 Payment Required"));
        assert!(is_account_terminal_error("This organization has been disabled."));
        // Terminal errors still count as usage-limit errors overall.
        assert!(is_usage_limit_error("Your credit balance is too low"));
    }
}
