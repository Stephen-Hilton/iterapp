use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
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

/// Seconds between probe pokes while the engine is working; also the fallback
/// retry interval after a usage-limit 429 that stated no reset time. Fixed at
/// the 2026-08-27 settings audit — it was config nobody had a reason to tune.
pub const PROBE_INTERVAL_SEC: u64 = 300;

/// Warn when the engine is working but the usage snapshot is older than this.
pub const SNAPSHOT_STALE_WARN_SEC: u64 = 900;

/// Model for the probe session — cheapest available.
const PROBE_MODEL: &str = "haiku";

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

/// When the usage window reopens, per the error the API returned. Two shapes
/// exist in the wild and both are read here:
///
/// - an epoch stamp — "Claude AI usage limit reached|1765500000";
/// - prose in a named zone — "You've hit your session limit · resets 4:10am
///   (America/Los_Angeles)", which is what the 429 payload actually carried on
///   2026-08-25 and what the epoch-only parser missed, dropping the engine onto
///   a 5-minute blind retry for the two hours until the real reset.
///
/// Prose is tried FIRST: it is an explicit statement of the reset, while the
/// epoch path is a digit-run heuristic over a JSON blob that also carries
/// durations, ids, and token counts, any of which could coincidentally look
/// like a plausible timestamp.
pub fn parse_reset_epoch(error: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_reset_prose(error, now).or_else(|| parse_reset_stamp(error, now))
}

/// Case-insensitive substring search returning a byte offset into `hay` itself,
/// so the caller can keep slicing the ORIGINAL text — an IANA zone name only
/// parses with its capitals intact ("America/Los_Angeles"), so lowercasing the
/// haystack to search it would destroy the very field being looked for.
fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    let last = h.len().checked_sub(n.len())?;
    (from..=last).find(|&i| hay.is_char_boundary(i) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// "resets 4:10am (America/Los_Angeles)" / "your limit will reset at 22:00
/// (Europe/Berlin)" → that clock time's next occurrence in that zone, strictly
/// in the future. The zone is required: a bare "resets 3am" names an instant
/// nobody here can locate, and guessing one wrong holds the engine for hours or
/// releases it into another 429.
fn parse_reset_prose(error: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut from = 0;
    while let Some(hit) = find_ci(error, "reset", from) {
        from = hit + "reset".len();
        if let Some(t) = parse_reset_clause(&error[from..], now) {
            return Some(t);
        }
    }
    None
}

/// The text immediately after "reset": an optional plural/preposition, a clock
/// time (12- or 24-hour), and a parenthesized IANA zone.
fn parse_reset_clause(rest: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let s = rest.strip_prefix('s').unwrap_or(rest).trim_start();
    let s = match s.get(..2) {
        Some(at) if at.eq_ignore_ascii_case("at") && s[2..].starts_with(char::is_whitespace) => s[2..].trim_start(),
        _ => s,
    };
    // Hour: one or two digits, and no more — a longer run is some other number.
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 2 {
        return None;
    }
    let hour: u32 = s[..digits].parse().ok()?;
    let mut s = &s[digits..];
    let mut minute = 0u32;
    if let Some(after_colon) = s.strip_prefix(':') {
        let mm = after_colon.get(..2)?;
        if !mm.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        minute = mm.parse().ok()?;
        s = &after_colon[2..];
    }
    let s = s.trim_start();
    // "4pm" is 16:00 and "12am" is midnight; without a marker the hour is
    // already 24-hour.
    let (hour, s) = match s.get(..2) {
        Some(m) if m.eq_ignore_ascii_case("am") => (if hour == 12 { 0 } else { hour }, &s[2..]),
        Some(m) if m.eq_ignore_ascii_case("pm") => (if hour == 12 { 12 } else { hour + 12 }, &s[2..]),
        _ => (hour, s),
    };
    let inner = s.trim_start().strip_prefix('(')?;
    let tz: Tz = inner[..inner.find(')')?].trim().parse().ok()?;
    next_occurrence(now, tz, NaiveTime::from_hms_opt(hour, minute, 0)?)
}

/// First instant strictly after `now` at which the clock in `tz` reads `at`.
/// Walks forward a day at a time, so a DST gap (a local time that never existed
/// that morning) rolls to the next day that has it — the same shape as
/// itersched's occurrence walk, run the other direction.
fn next_occurrence(now: DateTime<Utc>, tz: Tz, at: NaiveTime) -> Option<DateTime<Utc>> {
    let mut date = now.with_timezone(&tz).date_naive();
    for _ in 0..3 {
        if let Some(local) = tz.from_local_datetime(&date.and_time(at)).earliest() {
            let utc = local.with_timezone(&Utc);
            if utc > now {
                return Some(utc);
            }
        }
        date = date.succ_opt()?;
    }
    None
}

/// Extract a unix-epoch reset time from a usage-limit error, e.g.
/// "Claude AI usage limit reached|1765500000". Any 10-digit run that parses to a
/// timestamp between now and now+8 days counts.
fn parse_reset_stamp(error: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
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

// -------------------------------------------------------------------- the hold

/// The engine's answer to a 429: pick nothing until the window reopens.
///
/// Measured failure this replaces (pdy-dev, 2026-08-25T09:15Z): the hold was set
/// at 09:15:15 and lifted at 09:15:16, then re-set and re-lifted every ~5s for
/// hours, spawning a doomed agent each round. The early-lift trusted the
/// statusline snapshot — which read 37% — over the 429 the API had just
/// returned one second earlier. Those two numbers measure DIFFERENT limit
/// dimensions (the snapshot reports the 5h/7d account windows; the 429 that
/// blocked the call was a session limit), so a low snapshot is no evidence at
/// all that the blocked call would now succeed. Hence `authoritative`.
#[derive(Debug, Clone, PartialEq)]
pub struct Hold {
    /// No picking before this instant.
    pub until: DateTime<Utc>,
    /// True when `until` is the reset time the API itself stated. Such a hold
    /// runs its full course — nothing lifts it early. False when `until` is the
    /// blind fallback interval, which a genuinely fresh snapshot may cut short.
    pub authoritative: bool,
    /// When the hold began. A snapshot older than this predates the 429 and
    /// cannot speak to it.
    pub set_at: DateTime<Utc>,
}

impl Hold {
    /// Build the hold a usage-window error calls for. `fallback_sec` is used
    /// only when the error states no reset time.
    pub fn from_error(error: &str, now: DateTime<Utc>, fallback_sec: i64) -> Hold {
        match parse_reset_epoch(error, now) {
            Some(until) => Hold { until, authoritative: true, set_at: now },
            None => Hold {
                until: now + chrono::Duration::seconds(fallback_sec.max(1)),
                authoritative: false,
                set_at: now,
            },
        }
    }

    /// May a usage snapshot end this hold before `until`? Only for a guessed
    /// hold, and only on a snapshot taken strictly AFTER the 429 — i.e. one
    /// that has actually seen the account since it was blocked — showing
    /// utilization back under the full-stop band. An authoritative hold always
    /// answers false.
    pub fn may_lift_early(&self, usage: Option<&Usage>, now: DateTime<Utc>) -> bool {
        if self.authoritative {
            return false;
        }
        usage.is_some_and(|u| u.ts > self.set_at && u.effective_pct(now) < 95.0)
    }
}

/// A UTC instant written in the user's own timezone (globalsettings.
/// user_timezone — the same zone the webapp prints), so the one log line a hold
/// gets names a time the reader can compare to their own clock. The date rides
/// along because a reset is routinely tomorrow.
pub fn local_label(t: DateTime<Utc>, cfg: &Config) -> String {
    let tz: Tz = cfg.globalsettings.user_timezone.trim().parse().unwrap_or(chrono_tz::UTC);
    t.with_timezone(&tz).format("%-I:%M%P %Z (%a %-d %b)").to_string()
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
        let model = PROBE_MODEL;
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
        assert!(
            parse_reset_epoch("You've hit your limit · resets 3am", now).is_none(),
            "a clock time with no zone names no instant"
        );
        let past = now.timestamp() - 7200;
        assert!(parse_reset_epoch(&format!("limit reached|{}", past), now).is_none(), "past epochs rejected");
        assert!(parse_reset_epoch("error 4291234567890123 tokens", now).is_none(), "long digit runs are not epochs");
    }

    /// The phrasing that actually blocked pdy-dev for a night. Fixed clock: the
    /// engine ran at 09:15Z = 02:15 PDT, so "4:10am (America/Los_Angeles)" is
    /// later the SAME morning — 11:10Z, a ~2h hold, not the 5-minute guess the
    /// epoch-only parser fell back to.
    #[test]
    fn prose_reset_times_parse_in_their_own_zone() {
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 9, 15, 15).unwrap();
        let payload = r#"exit 1: {"api_error_status":429,"result":"You've hit your session limit · resets 4:10am (America/Los_Angeles)","type":"result"}"#;
        let hit = parse_reset_epoch(payload, now).expect("prose reset parses");
        assert_eq!(hit, Utc.with_ymd_and_hms(2026, 8, 25, 11, 10, 0).unwrap());

        // Same clock time already past today rolls to tomorrow: at 12:15 PDT,
        // 4:10am is behind us, so the next one is the following morning.
        let afternoon = Utc.with_ymd_and_hms(2026, 8, 25, 19, 15, 0).unwrap();
        let hit = parse_reset_epoch(payload, afternoon).expect("prose reset parses");
        assert_eq!(hit, Utc.with_ymd_and_hms(2026, 8, 26, 11, 10, 0).unwrap());

        // pm, bare hours, "reset at", and 24-hour clocks.
        let pm = parse_reset_epoch("5-hour limit reached ∙ resets 6pm (America/Los_Angeles)", now).unwrap();
        assert_eq!(pm, Utc.with_ymd_and_hms(2026, 8, 26, 1, 0, 0).unwrap(), "6pm PDT = 01:00Z next day");
        let at = parse_reset_epoch("Your limit will reset at 22:00 (Europe/Berlin)", now).unwrap();
        assert_eq!(at, Utc.with_ymd_and_hms(2026, 8, 25, 20, 0, 0).unwrap(), "22:00 CEST = 20:00Z");
        // Noon and midnight are the two the 12-hour clock gets wrong.
        let midnight = parse_reset_epoch("resets 12am (UTC)", now).unwrap();
        assert_eq!(midnight, Utc.with_ymd_and_hms(2026, 8, 26, 0, 0, 0).unwrap());
        let noon = parse_reset_epoch("resets 12pm (UTC)", now).unwrap();
        assert_eq!(noon, Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap());

        // Non-times near the word must not become holds.
        assert!(parse_reset_epoch("reset the session (America/Los_Angeles)", now).is_none());
        assert!(parse_reset_epoch("resets 4:10am (Mars/Olympus)", now).is_none(), "unknown zone");
        assert!(parse_reset_epoch("resets 4:10am", now).is_none(), "no zone, no instant");
    }

    /// The regression the whole issue is about: a 429-derived hold outlives a
    /// snapshot that says everything is fine, because the snapshot measures a
    /// different limit than the one that blocked the call.
    #[test]
    fn an_authoritative_hold_survives_a_fresh_low_snapshot() {
        let now = Utc.with_ymd_and_hms(2026, 8, 25, 9, 15, 15).unwrap();
        let rosy = Usage {
            ts: now + chrono::Duration::seconds(1), // newer than the hold — and still not evidence
            five_hour_pct: 37.0,
            five_hour_resets_at: None,
            seven_day_pct: 12.0,
            seven_day_resets_at: None,
        };

        let parsed = Hold::from_error("resets 4:10am (America/Los_Angeles)", now, 300);
        assert!(parsed.authoritative);
        assert_eq!(parsed.until, Utc.with_ymd_and_hms(2026, 8, 25, 11, 10, 0).unwrap());
        assert!(!parsed.may_lift_early(Some(&rosy), now), "a stated reset time is not up for negotiation");

        // No reset in the error: the fallback interval, which a snapshot taken
        // AFTER the 429 may cut short.
        let guessed = Hold::from_error("You've hit your session limit", now, 300);
        assert!(!guessed.authoritative);
        assert_eq!(guessed.until, now + chrono::Duration::seconds(300));
        assert!(guessed.may_lift_early(Some(&rosy), now));
        let stale = Usage { ts: now - chrono::Duration::seconds(1), ..rosy.clone() };
        assert!(!guessed.may_lift_early(Some(&stale), now), "a snapshot predating the 429 saw nothing");
        let hot = Usage { five_hour_pct: 99.0, ..rosy.clone() };
        assert!(!guessed.may_lift_early(Some(&hot), now), "still at the ceiling");
        assert!(!guessed.may_lift_early(None, now), "no snapshot lifts nothing");
    }

    #[test]
    fn local_label_names_a_time_the_reader_can_check() {
        let mut cfg = Config::default();
        cfg.globalsettings.user_timezone = "America/Los_Angeles".into();
        let label = local_label(Utc.with_ymd_and_hms(2026, 8, 25, 11, 10, 0).unwrap(), &cfg);
        assert!(label.starts_with("4:10am"), "got {}", label);
        assert!(label.contains("25 Aug"), "the date rides along: {}", label);
        // An unparseable zone falls back to UTC rather than panicking.
        cfg.globalsettings.user_timezone = "Nowhere/Atall".into();
        assert!(local_label(Utc.with_ymd_and_hms(2026, 8, 25, 11, 10, 0).unwrap(), &cfg).starts_with("11:10am"));
    }
}
