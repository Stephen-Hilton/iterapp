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
pub fn is_usage_limit_error(error: &str) -> bool {
    let e = error.to_lowercase();
    ["usage limit", "credit balance", "out of credit", "quota exceeded", "limit reached", "hit your limit"]
        .iter()
        .any(|needle| e.contains(needle))
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
}
