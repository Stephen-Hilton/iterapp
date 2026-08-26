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

/// Engine-side tunables that survived the structureV2 settings audit. All
/// PATHING settings moved to the two head files (`.iter/config.iter.json` +
/// `main.iter.md` — see project.rs): the old code_root / usecase_default_path /
/// interface_default_path / global_bizreq_path / global_techreq_path are gone
/// ({topdir}, {globalusecasedir}, {globalinterfacedir}, and globalcontextfiles
/// replaced them).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalSettings {
    /// IANA timezone (e.g. "America/Los_Angeles") used by the webapp to display
    /// timestamps. Data stays UTC on disk; this is display-only.
    pub user_timezone: String,
    /// Testwriter output bounds: how many tests a testwriter agent produces per
    /// testgroup (floor / ceiling). Read by agents from config.json directly.
    #[serde(alias = "test_min")]
    pub testwriter_min_tests_per_group: u32,
    #[serde(alias = "test_max")]
    pub testwriter_max_tests_per_group: u32,
    /// Max critical-review rounds per work item — substituted into the shared
    /// agent instructions (`{critreview_max_rounds}` in _shared.md).
    pub critreview_max_rounds: u32,
    /// The per-component test directory name (relative to a component's root).
    /// Testwriter items scope their codepath/lock to `<component>/<test_dir>`
    /// and code items list `<test_dir>/` in codepath_ignore so the two can run
    /// in parallel. Exported to agents as ITER_TEST_DIR.
    pub test_dir: String,
    /// Automation mode new work items get when nothing else decides
    /// (features/workitem_automation.md). "auto" — a user-filed item's children
    /// are born `queued`; "review" — they are born `todo` behind a human gate.
    /// Agent-created items still INHERIT their creating parent's mode first;
    /// this only fills the blank at the top of a lineage, which used to be
    /// hard-coded to "review" and stalled automated queues at every stage.
    pub default_automation: String,
    /// Where agents put scratch files, exported to them as $ITER_TEMP.
    /// Placeholder-expanded; see `temp_dir()` for the one binding that differs
    /// from the global placeholder table.
    pub temp_dir: String,
    /// Temp files are auto-removed after this many days (`iter tempsweep`,
    /// run daily by a seeded schedule). 0 = never sweep.
    pub temp_file_ttl_days: u64,
    pub log_default_path: String,
    pub log_level: String,
    pub log_max_size_mb: u64,
    pub log_max_files: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        GlobalSettings {
            user_timezone: "UTC".into(),
            testwriter_min_tests_per_group: 20,
            testwriter_max_tests_per_group: 100,
            critreview_max_rounds: 3,
            test_dir: "test".into(),
            default_automation: "auto".into(),
            temp_dir: "{dotiter}/temp/".into(),
            temp_file_ttl_days: 14,
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

/// The base directory relative codepaths, context patterns, and testfiles
/// resolve against — structureV2's `{topdir}` (the parent of `.iter/` unless
/// config.iter.json says otherwise). Replaces the old code_root setting.
pub fn code_root(project_root: &Path, _cfg: &Config) -> std::path::PathBuf {
    crate::project::Project::load(project_root).topdir
}

/// Directory where NEW use-case files are created — `{globalusecasedir}`.
pub fn usecase_dir(project_root: &Path, _cfg: &Config) -> std::path::PathBuf {
    crate::project::Project::load(project_root).usecasedir
}

/// Directory where NEW interface files are created — `{globalinterfacedir}`.
pub fn interface_dir(project_root: &Path, _cfg: &Config) -> std::path::PathBuf {
    crate::project::Project::load(project_root).interfacedir
}

/// Absolute temp directory for this project — the one place agents write
/// scratch files, handed to them as `$ITER_TEMP`. Defaults to `{dotiter}/temp/`:
/// scratch is PER-PROJECT, so it belongs in this project's engine home. Note it
/// is `{dotiter}`, not `{iterdir}` — a deployed binary is routinely shared by
/// several projects, so a temp directory next to the executable would pile
/// every project's scratch into one place, which is the wrong-directory problem
/// this setting exists to end.
///
/// A relative setting resolves against the project root. The directory is NOT
/// created here; callers that hand the path to an agent create it.
pub fn temp_dir(project_root: &Path, cfg: &Config) -> std::path::PathBuf {
    let raw = cfg.globalsettings.temp_dir.trim();
    let raw = if raw.is_empty() { "{dotiter}/temp/" } else { raw };
    let project = crate::project::Project::load(project_root);
    let expanded = project.vars().expand(raw).pop().unwrap_or_default();
    let expanded = expanded.trim_end_matches('/');
    let path = std::path::PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        project.root.join(path)
    }
}

/// Every file pinned into ALL new agent context: main.iter.md first, then the
/// resolved `globalcontextfiles` globs (which absorbed the old global
/// bizreq/techreq settings).
pub fn global_context_files(project_root: &Path) -> Vec<std::path::PathBuf> {
    crate::project::Project::load(project_root).context_files()
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

    /// The two settings other parts of the system now read by name; a typo in
    /// either default is a silent behavior change, so they are pinned here.
    #[test]
    fn automation_and_temp_defaults() {
        let g = GlobalSettings::default();
        assert_eq!(g.default_automation, "auto", "user-filed lineages automate unless told otherwise");
        assert_eq!(g.temp_dir, "{dotiter}/temp/");
        assert_eq!(g.temp_file_ttl_days, 14);
    }

    #[test]
    fn temp_dir_defaults_under_the_project_engine_home() {
        let dir = std::env::temp_dir().join(format!("iter-cfg-temp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let root = dir.canonicalize().unwrap();

        let cfg = Config::default();
        assert_eq!(temp_dir(&dir, &cfg), root.join(".iter/temp"));

        // An absolute setting is taken as written; a relative one hangs off the
        // project root, never the caller's cwd (issue 9's stray `.iter/temp/`).
        let mut cfg = Config::default();
        cfg.globalsettings.temp_dir = "/var/scratch/iter/".into();
        assert_eq!(temp_dir(&dir, &cfg), std::path::PathBuf::from("/var/scratch/iter"));
        cfg.globalsettings.temp_dir = "scratch/".into();
        assert_eq!(temp_dir(&dir, &cfg), root.join("scratch"));
        // Blank falls back to the default rather than resolving to the root.
        cfg.globalsettings.temp_dir = "  ".into();
        assert_eq!(temp_dir(&dir, &cfg), root.join(".iter/temp"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_root_is_topdir() {
        let dir = std::env::temp_dir().join(format!("iter-cfg-topdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let cfg = Config::default();
        assert_eq!(code_root(&dir, &cfg), dir.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
