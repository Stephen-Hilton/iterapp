use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EngineConfig {
    /// Scheduler pacing. Deliberately NOT in the Settings UI (2026-08-27
    /// settings audit): no user reason to tune them, but the e2e harness sets a
    /// 1-second tick to keep the suite fast, so they stay config, not consts.
    pub tick_interval_sec: u64,
    pub agent_stagger_ms: u64,
    pub codepath_lock_timeout_sec: u64,
    pub codepath_conflict_backoff_sec: u64,
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
            codepath_lock_timeout_sec: 3600,
            codepath_conflict_backoff_sec: 15,
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
    /// IANA timezone (e.g. "America/Los_Angeles") for DISPLAY ONLY: webapp
    /// timestamps and human-facing log labels. All engine math is UTC — data
    /// stays UTC on disk, and a schedule with no tz of its own runs in UTC
    /// (2026-08-27 decision; it used to fall back to this setting).
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
    /// The base concurrent-agent cap — the below-80% rung of the utilization
    /// ladder below. Moved here from the engine section (2026-08-27 settings
    /// audit) so the whole ladder lives together; `load()` carries an old
    /// config.json's engine.max_total_agents over.
    pub max_total_agents: usize,
    /// Agent caps by account utilization percent (max of the 5h and 7d windows).
    /// Below 80% max_total_agents applies unchanged; 0 = stop picking.
    pub max_agents_at_80: usize,
    pub max_agents_at_90: usize,
    pub max_agents_at_95: usize,
    /// Run the background probe session (tmux/screen + claude) that keeps the
    /// usage snapshot fresh. Off by default — it spawns a real claude session.
    pub probe_enabled: bool,
    /// Machine-wide snapshot written by the statusline collector. Account state is
    /// global, so one snapshot serves every iter project on the box.
    pub snapshot_path: String,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            max_total_agents: 8,
            max_agents_at_80: 4,
            max_agents_at_90: 2,
            max_agents_at_95: 0,
            probe_enabled: false,
            snapshot_path: "~/.claude/iter-usage-snapshot.json".into(),
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
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(mut v) => {
                // max_total_agents moved engine → limits (2026-08-27). A file
                // written before the move still names the cap in engine; honor
                // it unless limits already states its own.
                if v.is_object() && v.get("limits").and_then(|l| l.get("max_total_agents")).is_none() {
                    if let Some(old) = v.get("engine").and_then(|e| e.get("max_total_agents")).cloned() {
                        v["limits"]["max_total_agents"] = old;
                    }
                }
                match serde_json::from_value::<Config>(v) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        eprintln!("warning: {} is invalid ({}); using defaults", path.display(), e);
                        Config::default()
                    }
                }
            }
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
        assert_eq!(cfg.limits.max_total_agents, 8);
        assert_eq!(cfg.globalsettings.testwriter_min_tests_per_group, 20);
    }

    /// A config.json from before the 2026-08-27 audit names the agent cap as
    /// engine.max_total_agents; the load shim must carry a customized value
    /// into limits rather than silently resetting it to the default.
    #[test]
    fn old_engine_max_total_agents_moves_to_limits() {
        let dir = std::env::temp_dir().join(format!("iter-cfg-mta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(engine_dir(&dir)).unwrap();
        let path = engine_dir(&dir).join("config.json");
        std::fs::write(&path, r#"{"engine":{"max_total_agents":3}}"#).unwrap();
        assert_eq!(load(&dir).limits.max_total_agents, 3, "old location honored");
        // An explicit new-location value wins over a stale old one.
        std::fs::write(&path, r#"{"engine":{"max_total_agents":3},"limits":{"max_total_agents":5}}"#).unwrap();
        assert_eq!(load(&dir).limits.max_total_agents, 5);
        let _ = std::fs::remove_dir_all(&dir);
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
