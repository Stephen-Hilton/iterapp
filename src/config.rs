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
    /// Concurrent exec:"shell" work items (engine-run commands, no LLM). Separate
    /// from agent slots: shell runs are cheap and must not starve behind them.
    pub max_shell_workers: usize,
    /// Wall-clock budget per shell command in an exec:"shell" item; overrun kills
    /// the command and fails the item (normal attempt/backoff rules apply).
    pub shell_timeout_sec: u64,
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
            max_shell_workers: 2,
            shell_timeout_sec: 3600,
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
    /// Testwriter output bounds: how many tests a testwriter agent produces per
    /// testgroup (floor / ceiling). Read by agents from config.json directly.
    #[serde(alias = "test_min")]
    pub testwriter_min_tests_per_group: u32,
    #[serde(alias = "test_max")]
    pub testwriter_max_tests_per_group: u32,
    /// Max critical-review rounds per work item — substituted into the shared
    /// agent instructions (`{critreview_max_rounds}` in _shared.md), so the
    /// cap agents obey is a setting, not prompt-frozen prose. Each round is a
    /// synchronous critic session (minutes of wall clock plus spend).
    pub critreview_max_rounds: u32,
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
    /// The GLOBAL (project-wide) business/technical requirement files — the two
    /// documents surfaced to EVERY work item's spin-up. Exactly these files, never
    /// a directory scan, so stray docs beside them can't balloon agent context.
    /// `{codepath}` expands to the resolved code root; `~` and relative-to-engine-
    /// home work like the other path settings. Exported to agents as ITER_BIZREQ /
    /// ITER_TECHREQ; their directory is `$ITER_REQS` / `{reqs}` in context patterns.
    pub global_bizreq_path: String,
    pub global_techreq_path: String,
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
            testwriter_min_tests_per_group: 20,
            testwriter_max_tests_per_group: 100,
            critreview_max_rounds: 3,
            test_dir: "test".into(),
            usecase_default_path: "{codepath}/usecases/".into(),
            interface_default_path: "{codepath}/interfaces/".into(),
            global_bizreq_path: "{codepath}/reqs/bizreq.iter.md".into(),
            global_techreq_path: "{codepath}/reqs/techreq.iter.md".into(),
            log_default_path: "./logs/{YYYYMMDD-hh}.log".into(),
            log_level: "info".into(),
            log_max_size_mb: 10,
            log_max_files: 50,
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

/// Absolute path of the GLOBAL business requirements file
/// (`globalsettings.global_bizreq_path`, default `{codepath}/reqs/bizreq.iter.md`).
pub fn global_bizreq(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    default_dir(project_root, cfg, &cfg.globalsettings.global_bizreq_path, "{codepath}/reqs/bizreq.iter.md")
}

/// Absolute path of the GLOBAL technical requirements file
/// (`globalsettings.global_techreq_path`, default `{codepath}/reqs/techreq.iter.md`).
pub fn global_techreq(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    default_dir(project_root, cfg, &cfg.globalsettings.global_techreq_path, "{codepath}/reqs/techreq.iter.md")
}

/// The global requirement files that exist on disk, bizreq first — what the engine
/// surfaces to every work item's spin-up. Exactly these two paths (the settings),
/// never a directory scan.
pub fn global_req_files(project_root: &Path, cfg: &Config) -> Vec<std::path::PathBuf> {
    [global_bizreq(project_root, cfg), global_techreq(project_root, cfg)]
        .into_iter()
        .filter(|p| p.is_file())
        .collect()
}

/// The project-wide reqs directory: where the global bizreq lives. Kept for the
/// `{reqs}` placeholder in context/testfiles patterns and the `$ITER_REQS` env var
/// — the precise file paths are `global_bizreq`/`global_techreq` ($ITER_BIZREQ /
/// $ITER_TECHREQ); nothing scans this directory.
pub fn reqs_dir(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    let biz = global_bizreq(project_root, cfg);
    biz.parent().map(|p| p.to_path_buf()).unwrap_or(biz)
}

/// Expand a path setting: `{codepath}` → resolved code root, `~` → HOME, and a
/// relative value resolves against the engine home. Empty falls back.
fn default_dir(project_root: &Path, cfg: &Config, raw: &str, fallback: &str) -> std::path::PathBuf {
    let raw = raw.trim();
    let raw = if raw.is_empty() { fallback } else { raw };
    let mut expanded = raw.replace("{codepath}", &code_root(project_root, cfg).to_string_lossy());
    if let Some(home) = std::env::var_os("HOME") {
        if let Some(rest) = expanded.strip_prefix("~/") {
            expanded = format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
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
        assert_eq!(cfg.globalsettings.testwriter_min_tests_per_group, 20);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let cfg = load(Path::new("/nonexistent/nowhere"));
        assert_eq!(cfg.engine.tick_interval_sec, 5);
        assert_eq!(cfg.engine.max_attempts, 3);
    }

    #[test]
    fn global_req_paths_default_to_codepath_reqs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let cfg = Config::default();
        let code = code_root(&root, &cfg);
        assert_eq!(global_bizreq(&root, &cfg), code.join("reqs/bizreq.iter.md"));
        assert_eq!(global_techreq(&root, &cfg), code.join("reqs/techreq.iter.md"));
        assert_eq!(reqs_dir(&root, &cfg), code.join("reqs"));
    }

    #[test]
    fn global_req_paths_honor_settings() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let mut cfg = Config::default();
        cfg.globalsettings.global_bizreq_path = "docs/biz.md".into();
        cfg.globalsettings.global_techreq_path = "/abs/tech.md".into();
        assert_eq!(global_bizreq(&root, &cfg), root.join("docs/biz.md"));
        assert_eq!(global_techreq(&root, &cfg), Path::new("/abs/tech.md"));
        assert_eq!(reqs_dir(&root, &cfg), root.join("docs"));
    }

    #[test]
    fn global_req_files_lists_only_existing() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let mut cfg = Config::default();
        cfg.globalsettings.global_bizreq_path = "{codepath}/bizreq.md".into();
        cfg.globalsettings.global_techreq_path = "{codepath}/missing-techreq.md".into();
        let files = global_req_files(&root, &cfg);
        assert_eq!(files.len(), 1, "only the existing file surfaces: {:?}", files);
        assert!(files[0].ends_with("bizreq.md"));
    }
}
