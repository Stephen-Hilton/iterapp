use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EngineConfig {
    pub tick_interval_sec: u64,
    pub agent_stagger_ms: u64,
    pub queue_lock_retry_ms: u64,
    pub queue_lock_break_sec: u64,
    pub codepath_lock_timeout_sec: u64,
    pub codepath_conflict_backoff_sec: u64,
    pub max_total_agents: usize,
    pub max_open_workitems: usize,
    pub retry_backoff_sec: u64,
    pub max_attempts: u32,
    /// Daily spend cap in USD, summed from each turn's total_cost_usd. 0 = off.
    /// At the cap the engine auto-drains (finishes in-flight, picks nothing new).
    pub max_cost_usd_per_day: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            tick_interval_sec: 5,
            agent_stagger_ms: 100,
            queue_lock_retry_ms: 50,
            queue_lock_break_sec: 60,
            codepath_lock_timeout_sec: 3600,
            codepath_conflict_backoff_sec: 15,
            max_total_agents: 8,
            max_open_workitems: 200,
            retry_backoff_sec: 300,
            max_attempts: 3,
            max_cost_usd_per_day: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalSettings {
    /// IANA timezone (e.g. "America/Los_Angeles") used by the webapp to display
    /// timestamps. Data stays UTC on disk; this is display-only.
    pub user_timezone: String,
    /// Base directory for resolving relative codepaths, context patterns, and
    /// testfiles. Relative values (including the default ".") resolve against the
    /// engine home — the directory holding .iter/. Supports ~. Set this (e.g. "..")
    /// when iter lives in a subdirectory of the codebase it works on.
    pub code_root: String,
    pub test_min: u32,
    pub test_max: u32,
    pub test_default_path: String,
    /// The per-component test directory name (relative to a component's root).
    /// Definitive, not a guess: testwriter items scope their codepath/lock to
    /// `<component>/<test_dir>` and code items list `<test_dir>/` in codepath_ignore
    /// so the two can run in parallel. Exported to agents as ITER_TEST_DIR.
    pub test_dir: String,
    /// Where NEW use-case files are created (webapp CRUD, starter seed). Creation
    /// only — the scanner finds `*usecase.iter.md` anywhere, so there is no
    /// preferred read location. `{codepath}` expands to the resolved code root;
    /// a relative path resolves against the engine home.
    #[serde(alias = "usecase_default_location")]
    pub usecase_default_path: String,
    /// Where NEW interface files are created (`<id>.interface.iter.md`, one file
    /// per interface id — shared logical contracts, 1:M with C4 objects). Creation
    /// only — the scanner finds `*interface.iter.md` anywhere. Same expansion
    /// rules as `usecase_default_path`. Exported to agents as ITER_INTERFACE_DIR.
    pub interface_default_path: String,
    pub log_default_path: String,
    pub log_level: String,
    pub log_max_size_mb: u64,
    pub log_max_files: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        GlobalSettings {
            user_timezone: "UTC".into(),
            code_root: ".".into(),
            test_min: 20,
            test_max: 100,
            test_default_path: "./test*/".into(),
            test_dir: "test".into(),
            usecase_default_path: "{codepath}/usecases/".into(),
            interface_default_path: "{codepath}/interfaces/".into(),
            log_default_path: "./logs/{YYYYMMDD-hh}.log".into(),
            log_level: "info".into(),
            log_max_size_mb: 10,
            log_max_files: 50,
        }
    }
}

/// The deterministic test engine (see runtests.rs / testsweep.rs): every test is a
/// shell script; the sweep re-runs testgroups that aren't provably green and births
/// fix work items from red runs. Priorities are higher-is-sooner (default work is 5,
/// so sweep items fill idle capacity instead of starving user work).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TestingConfig {
    pub test_sweep_active: bool,
    /// Sweep cadence: how often the engine wakes to interrogate all testgroups.
    pub minutes_between_test_sweeps: u64,
    /// A group with a green run newer than this is left alone by the sweep.
    pub test_green_stale_hours: u64,
    /// Wall-clock budget for one testgroup's scripts, shared across the group;
    /// a script still running at the deadline is killed and recorded as `error`.
    pub test_sweep_timeout_minutes: u64,
    /// How many testgroups run concurrently during a sweep.
    pub parallel_test_concurrency: usize,
    /// Priority for fix items born from a group with no green run recorded.
    pub workitem_priority_lastrun_not_green: i64,
    /// Priority for fix items born from a group whose green run went stale.
    pub workitem_priority_lastrun_green: i64,
}

impl Default for TestingConfig {
    fn default() -> Self {
        TestingConfig {
            test_sweep_active: true,
            minutes_between_test_sweeps: 120,
            test_green_stale_hours: 24,
            test_sweep_timeout_minutes: 10,
            parallel_test_concurrency: 3,
            workitem_priority_lastrun_not_green: 4,
            workitem_priority_lastrun_green: 2,
        }
    }
}

/// Account-usage throttling (see limits.rs): tiered agent caps driven by Claude
/// Code's server-authoritative rate_limits percentages, kept fresh by a background
/// interactive probe session.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Run the background probe session (tmux/screen + claude) that keeps the
    /// usage snapshot fresh. Off by default — it spawns a real claude session.
    pub probe_enabled: bool,
    /// Agent caps by account utilization percent (max of the 5h and 7d windows).
    /// Below 80% the engine uses max_total_agents unchanged; 0 = stop picking.
    pub max_agents_at_80: usize,
    pub max_agents_at_90: usize,
    pub max_agents_at_95: usize,
    /// Seconds between probe pokes while the engine is working; also the retry
    /// interval after a hard usage-limit hit when no reset time was parseable.
    pub probe_interval_sec: u64,
    /// Warn when the engine is working but the snapshot is older than this.
    pub snapshot_stale_warn_sec: u64,
    /// Machine-wide snapshot written by the statusline collector. Account state is
    /// global, so one snapshot serves every iter project on the box.
    pub snapshot_path: String,
    /// Model for the probe session — cheapest available.
    pub probe_model: String,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            probe_enabled: false,
            max_agents_at_80: 4,
            max_agents_at_90: 2,
            max_agents_at_95: 0,
            probe_interval_sec: 300,
            snapshot_stale_warn_sec: 900,
            snapshot_path: "~/.claude/iter-usage-snapshot.json".into(),
            probe_model: "haiku".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Config {
    pub engine: EngineConfig,
    pub globalsettings: GlobalSettings,
    pub limits: LimitsConfig,
    pub testing: TestingConfig,
}

pub fn engine_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".iter").join(".engine")
}

/// Resolve globalsettings.code_root to the absolute directory that relative
/// codepaths/context/testfiles resolve against. `project_root` is the engine home
/// (where .iter/ lives); the default "." keeps historical behavior.
pub fn code_root(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    let raw = cfg.globalsettings.code_root.trim();
    let raw = if raw.is_empty() { "." } else { raw };
    let mut p = raw.to_string();
    if let Some(home) = std::env::var_os("HOME") {
        if let Some(rest) = p.strip_prefix("~/") {
            p = format!("{}/{}", home.to_string_lossy(), rest);
        } else if p == "~" {
            p = home.to_string_lossy().into_owned();
        }
    }
    let path = std::path::PathBuf::from(&p);
    let abs = if path.is_absolute() { path } else { project_root.join(path) };
    abs.canonicalize().unwrap_or(abs)
}

/// Directory where NEW use-case files are created
/// (`globalsettings.usecase_default_path`, default `{codepath}/usecases/`).
pub fn usecase_dir(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    default_dir(project_root, cfg, &cfg.globalsettings.usecase_default_path, "{codepath}/usecases/")
}

/// Directory where NEW interface files are created
/// (`globalsettings.interface_default_path`, default `{codepath}/interfaces/`).
pub fn interface_dir(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    default_dir(project_root, cfg, &cfg.globalsettings.interface_default_path, "{codepath}/interfaces/")
}

fn default_dir(project_root: &Path, cfg: &Config, raw: &str, fallback: &str) -> std::path::PathBuf {
    let raw = raw.trim();
    let raw = if raw.is_empty() { fallback } else { raw };
    let expanded = raw.replace("{codepath}", &code_root(project_root, cfg).to_string_lossy());
    let path = std::path::PathBuf::from(expanded);
    if path.is_absolute() { path } else { project_root.join(path) }
}

pub fn load(project_root: &Path) -> Config {
    let path = engine_dir(project_root).join("config.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("warning: {} is invalid ({}); using defaults", path.display(), e);
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_template_config() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let cfg = load(&root);
        assert_eq!(cfg.engine.max_open_workitems, 200);
        assert_eq!(cfg.engine.queue_lock_retry_ms, 50);
        assert_eq!(cfg.globalsettings.test_min, 20);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let cfg = load(Path::new("/nonexistent/nowhere"));
        assert_eq!(cfg.engine.tick_interval_sec, 5);
        assert_eq!(cfg.engine.max_attempts, 3);
    }
}
