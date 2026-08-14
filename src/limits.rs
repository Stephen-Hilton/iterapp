use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

use crate::config::{self, Config, LimitsConfig};
use crate::logging;

/// Account-usage throttling built on Claude Code's server-authoritative
/// `rate_limits` statusline data (v2.1.80+). A statusline collector script tees the
/// percentages into a machine-wide snapshot file; a background interactive probe
/// session (tmux or screen) keeps that snapshot fresh when nobody is typing; the
/// scheduler reads the snapshot each tick and applies tiered agent caps.
///
/// Design notes: the snapshot is account-scoped truth — a probe from ANY machine on
/// the account reflects the whole account's burn, so multiple engines coordinate for
/// free. Percentages come from the server; nothing here estimates spend locally.

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub ts: DateTime<Utc>,
    pub five_hour_pct: f64,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_pct: f64,
    pub seven_day_resets_at: Option<i64>,
}

impl Usage {
    /// Utilization used for throttling: the max of both windows, with any window
    /// whose reset time has already passed counted as 0 — so a stale snapshot
    /// cannot keep the engine throttled across a known reset boundary.
    pub fn effective_pct(&self, now: DateTime<Utc>) -> f64 {
        let adj = |pct: f64, resets: Option<i64>| match resets {
            Some(epoch) if now.timestamp() >= epoch => 0.0,
            _ => pct,
        };
        adj(self.five_hour_pct, self.five_hour_resets_at)
            .max(adj(self.seven_day_pct, self.seven_day_resets_at))
    }

    pub fn age_sec(&self, now: DateTime<Utc>) -> i64 {
        (now - self.ts).num_seconds()
    }
}

pub fn snapshot_path(cfg: &Config) -> PathBuf {
    let raw = cfg.limits.snapshot_path.trim();
    let raw = if raw.is_empty() { "~/.claude/iter-usage-snapshot.json" } else { raw };
    let mut p = raw.to_string();
    if let Some(home) = std::env::var_os("HOME") {
        if let Some(rest) = p.strip_prefix("~/") {
            p = format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    PathBuf::from(p)
}

/// Read the collector-written snapshot. None when missing or unparseable.
pub fn read_snapshot(cfg: &Config) -> Option<Usage> {
    let text = std::fs::read_to_string(snapshot_path(cfg)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let ts = crate::workitems::parse_iso(v.get("ts")?.as_str()?)?;
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

/// Tiered agent cap for a utilization percent. None = no override (below 80%).
pub fn tier_cap(l: &LimitsConfig, pct: f64) -> Option<usize> {
    if pct >= 95.0 {
        Some(l.max_agents_at_95)
    } else if pct >= 90.0 {
        Some(l.max_agents_at_90)
    } else if pct >= 80.0 {
        Some(l.max_agents_at_80)
    } else {
        None
    }
}

/// Extract a unix-epoch reset time from a usage-limit error, e.g.
/// "Claude AI usage limit reached|1765500000". Any 10-digit run that parses to a
/// timestamp between now and now+8 days counts; prose reset phrasings return None
/// and the caller falls back to its retry interval.
pub fn parse_reset_epoch(error: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut digits = String::new();
    let mut candidates: Vec<i64> = Vec::new();
    for ch in error.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if digits.len() == 10 {
                if let Ok(n) = digits.parse::<i64>() {
                    candidates.push(n);
                }
            }
            digits.clear();
        }
    }
    candidates
        .into_iter()
        .filter_map(|n| DateTime::from_timestamp(n, 0))
        .find(|t| *t > now && *t <= now + chrono::Duration::days(8))
}

// ------------------------------------------------------------------ probe session

const PROBE_SESSION: &str = "iter-probe";

fn probe_dir(project_root: &Path) -> PathBuf {
    config::engine_dir(project_root).join("probe")
}

#[derive(Clone, Copy)]
enum Mux {
    Tmux,
    Screen,
}

fn detect_mux() -> Option<Mux> {
    let runs = |bin: &str, arg: &str| {
        std::process::Command::new(bin)
            .arg(arg)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    };
    if runs("tmux", "-V") {
        Some(Mux::Tmux)
    } else if runs("screen", "-version") {
        // GNU screen exits nonzero on -version but spawning proves it exists.
        Some(Mux::Screen)
    } else {
        None
    }
}

fn shell(bin: &str, args: &[&str], cwd: Option<&Path>) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    cmd.output().ok()
}

fn session_exists(mux: Mux) -> bool {
    match mux {
        Mux::Tmux => shell("tmux", &["has-session", "-t", PROBE_SESSION], None)
            .map(|o| o.status.success())
            .unwrap_or(false),
        Mux::Screen => shell("screen", &["-ls"], None)
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!(".{}", PROBE_SESSION)))
            .unwrap_or(false),
    }
}

fn send_line(mux: Mux, text: &str) {
    match mux {
        Mux::Tmux => {
            let _ = shell("tmux", &["send-keys", "-t", PROBE_SESSION, "-l", text], None);
            let _ = shell("tmux", &["send-keys", "-t", PROBE_SESSION, "Enter"], None);
        }
        Mux::Screen => {
            let stuffed = format!("{}\r", text);
            let _ = shell("screen", &["-S", PROBE_SESSION, "-p", "0", "-X", "stuff", &stuffed], None);
        }
    }
}

/// Write the probe workspace: `.claude/settings.json` pointing the statusline at
/// the collector script (absolute paths, so it survives cwd changes), and make the
/// collector executable. Idempotent.
pub fn ensure_probe_setup(project_root: &Path, cfg: &Config) -> std::io::Result<PathBuf> {
    let dir = probe_dir(project_root);
    std::fs::create_dir_all(dir.join(".claude"))?;
    let collector = config::engine_dir(project_root).join("statusline-collector.py");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&collector) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o755);
            let _ = std::fs::set_permissions(&collector, perms);
        }
    }
    let command = format!("{} {}", collector.to_string_lossy(), snapshot_path(cfg).to_string_lossy());
    let settings = serde_json::json!({ "statusLine": { "type": "command", "command": command } });
    std::fs::write(dir.join(".claude/settings.json"), serde_json::to_string_pretty(&settings)?)?;
    Ok(dir)
}

/// One probe poke: ensure the multiplexed interactive session exists, then send a
/// minimal message so the next API response refreshes the statusline snapshot.
/// Runs on its own thread (creation involves multi-second sleeps).
pub fn probe_poke(project_root: &Path, cfg: &Config, poke_count: u64) {
    let Some(mux) = detect_mux() else {
        logging::warn("probe", "neither tmux nor screen found; usage probe disabled (install tmux)");
        return;
    };
    let dir = match ensure_probe_setup(project_root, cfg) {
        Ok(d) => d,
        Err(e) => {
            logging::warn("probe", &format!("cannot prepare probe workspace: {}", e));
            return;
        }
    };
    if !session_exists(mux) {
        let model = if cfg.limits.probe_model.trim().is_empty() { "haiku" } else { cfg.limits.probe_model.trim() };
        let created = match mux {
            Mux::Tmux => shell(
                "tmux",
                &["new-session", "-d", "-s", PROBE_SESSION, "-c", &dir.to_string_lossy(), "claude", "--model", model],
                None,
            )
            .map(|o| o.status.success())
            .unwrap_or(false),
            Mux::Screen => shell("screen", &["-dmS", PROBE_SESSION, "claude", "--model", model], Some(&dir))
                .map(|o| o.status.success())
                .unwrap_or(false),
        };
        if !created {
            logging::warn("probe", "could not create the probe session");
            return;
        }
        logging::info("probe", &format!("created background claude probe session ({})", match mux { Mux::Tmux => "tmux", Mux::Screen => "screen" }));
        // Give the TUI time to boot, then accept a possible first-run trust dialog.
        std::thread::sleep(std::time::Duration::from_secs(5));
        send_line(mux, "");
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    // Periodically clear the probe conversation so its context (and cost) stays tiny.
    if poke_count % 20 == 0 {
        send_line(mux, "/clear");
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    send_line(mux, "usage probe - reply with one word");
    logging::info("probe", &format!("poked usage probe (#{})", poke_count));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_snapshot(path: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.limits.snapshot_path = path.to_string_lossy().into_owned();
        cfg
    }

    #[test]
    fn snapshot_roundtrip_and_effective_pct() {
        let dir = std::env::temp_dir().join(format!("iter-limits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snap.json");
        let now = Utc::now();
        let future = now.timestamp() + 3600;
        std::fs::write(
            &path,
            format!(
                r#"{{"ts":"{}","rate_limits":{{"five_hour":{{"used_percentage":83,"resets_at":{}}},"seven_day":{{"used_percentage":41,"resets_at":{}}}}}}}"#,
                now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                future,
                future
            ),
        )
        .unwrap();
        let u = read_snapshot(&cfg_with_snapshot(&path)).expect("parses");
        assert_eq!(u.five_hour_pct, 83.0);
        assert_eq!(u.effective_pct(now), 83.0, "max of windows");
        // A window whose reset already passed counts as zero even if stale.
        let past = now.timestamp() - 10;
        std::fs::write(
            &path,
            format!(
                r#"{{"ts":"{}","rate_limits":{{"five_hour":{{"used_percentage":97,"resets_at":{}}},"seven_day":{{"used_percentage":41,"resets_at":{}}}}}}}"#,
                now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                past,
                future
            ),
        )
        .unwrap();
        let u = read_snapshot(&cfg_with_snapshot(&path)).expect("parses");
        assert_eq!(u.effective_pct(now), 41.0, "expired 5h window zeroes out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tier_caps_follow_thresholds() {
        let l = LimitsConfig::default();
        assert_eq!(tier_cap(&l, 12.0), None, "below 80: no override");
        assert_eq!(tier_cap(&l, 80.0), Some(l.max_agents_at_80));
        assert_eq!(tier_cap(&l, 90.0), Some(l.max_agents_at_90));
        assert_eq!(tier_cap(&l, 94.9), Some(l.max_agents_at_90));
        assert_eq!(tier_cap(&l, 95.0), Some(l.max_agents_at_95));
        assert_eq!(tier_cap(&l, 100.0), Some(0), "default at 95 is full stop");
    }

    #[test]
    fn reset_epoch_parses_only_plausible_futures() {
        let now = Utc::now();
        let future = now.timestamp() + 7200;
        let hit = parse_reset_epoch(&format!("Claude AI usage limit reached|{}", future), now).expect("epoch found");
        assert_eq!(hit.timestamp(), future);
        assert!(parse_reset_epoch("You've hit your limit · resets 3am", now).is_none(), "prose has no epoch");
        let past = now.timestamp() - 7200;
        assert!(parse_reset_epoch(&format!("limit reached|{}", past), now).is_none(), "past epochs rejected");
        assert!(parse_reset_epoch("error 4291234567890123 tokens", now).is_none(), "long digit runs are not epochs");
    }
}
