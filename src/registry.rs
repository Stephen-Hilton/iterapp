use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One row per running `iter start` server, in ~/.iterapp/servers.json. Rows are
/// advisory: readers drop any whose pid is no longer alive, so crashes never leave
/// permanent ghosts.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServerRow {
    pub project_name: String,
    pub url_slug: String,
    pub path: String,
    pub port: u16,
    pub pid: u32,
    pub started: String,
}

fn registry_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".iterapp").join("servers.json")
}

fn load() -> Vec<ServerRow> {
    let text = std::fs::read_to_string(registry_path()).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

/// All writes hold the record lock across load → modify → store, so concurrent
/// `iter start` processes can't clobber each other's rows.
fn with_lock(f: impl FnOnce(&mut Vec<ServerRow>)) {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _guard = crate::locks::acquire_file_lock(&path, 20, 10);
    let mut rows = load();
    f(&mut rows);
    if let Ok(text) = serde_json::to_string_pretty(&rows) {
        let _ = std::fs::write(&path, text);
    }
}

pub fn pid_alive(pid: u32) -> bool {
    // Absolute path: a server spawned with a stripped env (no PATH — e.g.
    // Playwright's webServer) must still see itself as alive.
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Register this server; replaces any stale row for the same project path.
pub fn register(project_root: &Path, project_name: &str, slug: &str, port: u16) {
    let path_str = project_root.to_string_lossy().into_owned();
    let row = ServerRow {
        project_name: project_name.to_string(),
        url_slug: slug.to_string(),
        path: path_str.clone(),
        port,
        pid: std::process::id(),
        started: crate::workitems::now_iso(),
    };
    with_lock(|rows| {
        rows.retain(|r| r.path != path_str && pid_alive(r.pid));
        rows.push(row);
    });
}

pub fn deregister() {
    let pid = std::process::id();
    with_lock(|rows| rows.retain(|r| r.pid != pid));
}

/// Live rows only (pid-checked).
pub fn live() -> Vec<ServerRow> {
    load().into_iter().filter(|r| pid_alive(r.pid)).collect()
}
