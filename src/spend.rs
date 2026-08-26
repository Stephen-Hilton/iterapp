use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-turn spend ledger: every agent turn's `total_cost_usd` and token counts from
/// the claude CLI's JSON output, appended to the `spend` table in the project
/// database. This is the same accounting Claude Code itself reports — the engine
/// just keeps the receipts, so a daily budget can auto-drain the loop before
/// credits run dry.
///
/// This type is still the serde shape of one ledger row because it is what
/// `iter export --table spend` emits and what the retired `spend.jsonl` held —
/// the export has to reproduce the file the import consumed.
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

/// Open the ledger, migrating `spend.jsonl` on the way in the first time. The
/// importer's own gate is a `stat` of the retired file, so once it is renamed
/// this costs nothing per turn — which matters, because both entry points below
/// run on every agent dispatch.
fn ledger(project_root: &Path) -> Option<rusqlite::Connection> {
    let mut conn = crate::db::open(project_root).ok()?;
    let _ = crate::db::import_spend_jsonl(&mut conn, project_root);
    Some(conn)
}

/// Receipt one agent turn. Best-effort by design: the ledger records what
/// already happened, so a write failure must never propagate into the turn that
/// succeeded — the same tolerance the append-to-file form had.
pub fn record(project_root: &Path, entry: &SpendEntry) {
    let Some(conn) = ledger(project_root) else { return };
    let _ = crate::db::record_spend(
        &conn,
        &entry.ts,
        &entry.workid,
        &entry.agent,
        &entry.turn,
        entry.usd,
        entry.input_tokens,
        entry.output_tokens,
    );
}

/// Total USD spent today (UTC), summed from the ledger.
pub fn today_usd(project_root: &Path) -> f64 {
    let prefix = chrono::Utc::now().format("%Y-%m-%d").to_string();
    sum_for_day(project_root, &prefix)
}

/// One day's spend as a `SUM(usd)` over the `ts` index, instead of parsing every
/// receipt ever written to answer a question about today.
pub fn sum_for_day(project_root: &Path, day_prefix: &str) -> f64 {
    ledger(project_root).map(|c| crate::db::spend_for_day(&c, day_prefix)).unwrap_or(0.0)
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

    fn tmpdir_named(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-spend-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter/.engine")).unwrap();
        dir
    }

    fn tmpdir() -> PathBuf {
        tmpdir_named("ledger")
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

    /// A project whose receipts predate the database keeps every dollar of them,
    /// and the day rollup spans the imported rows and the new ones as one ledger
    /// — the budget drain would otherwise reset to zero the day of the upgrade.
    #[test]
    fn existing_jsonl_receipts_import_once_and_keep_counting() {
        let root = tmpdir_named("import");
        let path = root.join(".iter/.engine/spend.jsonl");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let line = |ts: &str, usd: f64| {
            serde_json::to_string(&SpendEntry {
                ts: ts.into(),
                workid: "old".into(),
                usd,
                input_tokens: 7,
                ..Default::default()
            })
            .unwrap()
        };
        std::fs::write(
            &path,
            format!(
                "{}\n{}\nnot json at all\n{}\n",
                line(&format!("{}T09:00:00Z", today), 0.50),
                line(&format!("{}T09:30:00Z", today), 0.25),
                line("2020-01-01T00:00:00Z", 99.0)
            ),
        )
        .unwrap();

        // The first read imports; the file is renamed, never deleted.
        assert!((sum_for_day(&root, &today) - 0.75).abs() < 1e-9);
        assert!(!path.exists());
        assert!(root.join(".iter/.engine/spend.jsonl.imported").is_file());

        // Idempotent: nothing doubles on the next call, and new receipts add to
        // the imported ones rather than starting a second ledger.
        assert!((sum_for_day(&root, &today) - 0.75).abs() < 1e-9, "no double import");
        record(&root, &SpendEntry { ts: format!("{}T12:00:00Z", today), usd: 1.0, ..Default::default() });
        assert!((today_usd(&root) - 1.75).abs() < 1e-9);
        assert!((sum_for_day(&root, "2020-01-01") - 99.0).abs() < 1e-9, "history came along");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The escape hatch the move to a database costs us back: an exported
    /// ledger line is the same object `spend.jsonl` held, so the old greps and
    /// the old parsers still work against `iter export --table spend`.
    #[test]
    fn export_reproduces_the_ledger_line_shape() {
        let root = tmpdir_named("export");
        record(
            &root,
            &SpendEntry {
                ts: "2026-08-26T10:00:00Z".into(),
                workid: "critreview".into(),
                agent: "_critic".into(),
                turn: "attempt-1".into(),
                usd: 0.05,
                input_tokens: 10,
                output_tokens: 5,
            },
        );
        let conn = crate::db::open(&root).unwrap();
        let lines = crate::db::export_table(&conn, "spend", crate::db::ExportScope::All).unwrap();
        assert_eq!(lines.len(), 1);
        let back: SpendEntry = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(back.workid, "critreview");
        assert_eq!(back.agent, "_critic");
        assert_eq!(back.turn, "attempt-1");
        assert_eq!(back.input_tokens, 10);
        assert!((back.usd - 0.05).abs() < 1e-9);
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
