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
    pub test_min: u32,
    pub test_max: u32,
    pub test_default_path: String,
    pub log_default_path: String,
    pub log_level: String,
    pub log_max_size_mb: u64,
    pub log_max_files: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        GlobalSettings {
            test_min: 20,
            test_max: 100,
            test_default_path: "./test*/".into(),
            log_default_path: "./logs/{YYYYMMDD-hh}.log".into(),
            log_level: "info".into(),
            log_max_size_mb: 10,
            log_max_files: 50,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct Config {
    pub engine: EngineConfig,
    pub globalsettings: GlobalSettings,
}

pub fn engine_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".iter").join(".engine")
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
