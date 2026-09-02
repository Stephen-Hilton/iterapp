//! Usage% tracking — the V2 mechanism ported per-account (limits.rs +
//! statusline-collector.py). Claude Code pipes server-authoritative
//! rate_limits (5h/7d used_percentage + resets_at) to the configured
//! statusline command after each API response; the collector tees it into a
//! snapshot file the engine reads. In V3 every spawned agent session gets the
//! collector injected via --settings with a PER-ACCOUNT snapshot path, so
//! real work refreshes its own account's numbers for free.
//!
//! Snapshot dir: $ITER_USAGE_DIR (default ~/.claude). Files:
//!   iter3-usage-<account>.json  (account "" -> "default"; the V2 single-
//!   account snapshot iter-usage-snapshot.json is read as a fallback for
//!   "default" so an existing V2 probe keeps feeding V3.)

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;

const COLLECTOR: &str = include_str!("statusline-collector.py");

/// Warn when working but the snapshot is older than this (V2 constant).
pub const SNAPSHOT_STALE_WARN_SEC: i64 = 900;

pub fn usage_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ITER_USAGE_DIR") {
        if !d.trim().is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude")
}

fn account_key(account: &str) -> String {
    if account.trim().is_empty() { "default".into() } else { account.trim().to_string() }
}

pub fn snapshot_path(account: &str) -> PathBuf {
    usage_dir().join(format!("iter3-usage-{}.json", account_key(account)))
}

pub fn collector_path() -> PathBuf {
    usage_dir().join("iter3-statusline-collector.py")
}

/// Idempotently install the collector script so --settings can reference it.
pub fn install_collector() {
    let path = collector_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current != COLLECTOR {
        if std::fs::write(&path, COLLECTOR).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
}

/// The --settings JSON that wires a spawned session's statusline to the
/// collector, teeing rate_limits into this account's snapshot.
pub fn statusline_settings(account: &str) -> String {
    serde_json::json!({
        "statusLine": {
            "type": "command",
            "command": format!("python3 {} {}",
                collector_path().display(), snapshot_path(account).display()),
        }
    })
    .to_string()
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub ts: Option<DateTime<Utc>>,
    pub five_hour_pct: f64,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: f64,
    pub seven_day_resets_at: Option<i64>,
}

impl Usage {
    /// Effective utilization now: an expired window (resets_at in the past)
    /// reads as 0 — the V2 "expired 5h window zeroes out" rule.
    pub fn effective_pct(&self, now: DateTime<Utc>) -> f64 {
        let live = |pct: f64, resets: Option<i64>| -> f64 {
            match resets {
                Some(epoch) if (epoch as i64) < now.timestamp() => 0.0,
                _ => pct,
            }
        };
        live(self.five_hour_pct, self.five_hour_resets_at)
            .max(live(self.seven_day_pct, self.seven_day_resets_at))
    }

    pub fn age_sec(&self, now: DateTime<Utc>) -> Option<i64> {
        self.ts.map(|t| (now - t).num_seconds())
    }
}

fn parse_snapshot(text: &str) -> Option<Usage> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let ts = v
        .get("ts")
        .and_then(|t| t.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let rl = v.get("rate_limits")?;
    let win = |key: &str| -> (f64, Option<i64>) {
        let w = rl.get(key);
        (
            w.and_then(|w| w.get("used_percentage")).and_then(|p| p.as_f64()).unwrap_or(0.0),
            w.and_then(|w| w.get("resets_at")).and_then(|r| r.as_i64()),
        )
    };
    let (five_hour_pct, five_hour_resets_at) = win("five_hour");
    let (seven_day_pct, seven_day_resets_at) = win("seven_day");
    Some(Usage { ts, five_hour_pct, five_hour_resets_at, seven_day_pct, seven_day_resets_at })
}

pub fn read_usage(account: &str) -> Option<Usage> {
    let text = std::fs::read_to_string(snapshot_path(account)).ok().or_else(|| {
        // V2 compatibility: the old machine-wide snapshot feeds "default"
        if account_key(account) == "default" {
            std::fs::read_to_string(usage_dir().join("iter-usage-snapshot.json")).ok()
        } else {
            None
        }
    })?;
    parse_snapshot(&text)
}

/// account name -> effective pct (rounded), for the ladder + maxagents gates.
/// Missing snapshot = 0 (unknown usage never blocks; the stale warning is the
/// operator's signal to wire the collector).
pub fn usage_map(accounts: &[iter_core::Account], now: DateTime<Utc>) -> BTreeMap<String, u8> {
    let mut out = BTreeMap::new();
    for a in accounts {
        let pct = read_usage(&a.name).map(|u| u.effective_pct(now)).unwrap_or(0.0);
        out.insert(a.name.clone(), pct.round().clamp(0.0, 100.0) as u8);
    }
    out
}

/// Effective pct for a single (possibly unnamed/default) account.
pub fn effective_pct_for(account: &str, now: DateTime<Utc>) -> u8 {
    read_usage(account)
        .map(|u| u.effective_pct(now).round().clamp(0.0, 100.0) as u8)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_window_zeroes_out() {
        let now = Utc::now();
        let u = Usage {
            ts: Some(now),
            five_hour_pct: 41.0,
            five_hour_resets_at: Some(now.timestamp() - 10),
            seven_day_pct: 14.0,
            seven_day_resets_at: Some(now.timestamp() + 1000),
            ..Default::default()
        };
        assert_eq!(u.effective_pct(now), 14.0);
        let u2 = Usage { five_hour_resets_at: Some(now.timestamp() + 1000), ..u.clone() };
        assert_eq!(u2.effective_pct(now), 41.0);
    }

    #[test]
    fn snapshot_parses_collector_format() {
        let text = r#"{"ts":"2026-09-01T12:00:00Z","rate_limits":{
            "five_hour":{"used_percentage":30.0,"resets_at":99999999999},
            "seven_day":{"used_percentage":14.0,"resets_at":99999999999}}}"#;
        let u = parse_snapshot(text).unwrap();
        assert_eq!(u.five_hour_pct, 30.0);
        assert_eq!(u.seven_day_pct, 14.0);
        assert!(u.ts.is_some());
    }
}
