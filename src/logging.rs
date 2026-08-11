use chrono::Utc;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

struct Logger {
    level: Level,
    file: Option<Mutex<(PathBuf, u64)>>, // (path, max_bytes)
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init(level_name: &str, file_path: Option<PathBuf>, max_size_mb: u64) {
    let level = match level_name {
        "debug" => Level::Debug,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    };
    let file = file_path.map(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Mutex::new((p, max_size_mb * 1024 * 1024))
    });
    let _ = LOGGER.set(Logger { level, file });
}

pub fn log(level: Level, tag: &str, msg: &str) {
    let logger = LOGGER.get_or_init(|| Logger { level: Level::Info, file: None });
    if level < logger.level {
        return;
    }
    let name = match level {
        Level::Debug => "DEBUG",
        Level::Info => "INFO ",
        Level::Warn => "WARN ",
        Level::Error => "ERROR",
    };
    let line = format!("{} {} [{}] {}", Utc::now().format("%H:%M:%S"), name, tag, msg);
    println!("{}", line);
    if let Some(file) = &logger.file {
        if let Ok(guard) = file.lock() {
            let (path, max_bytes) = &*guard;
            // Naive single-slot rotation: at the size cap, current log becomes .1.
            if std::fs::metadata(path).map(|m| m.len() > *max_bytes).unwrap_or(false) {
                let _ = std::fs::rename(path, PathBuf::from(format!("{}.1", path.display())));
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
}

#[allow(dead_code)]
pub fn debug(tag: &str, msg: &str) {
    log(Level::Debug, tag, msg);
}
pub fn info(tag: &str, msg: &str) {
    log(Level::Info, tag, msg);
}
pub fn warn(tag: &str, msg: &str) {
    log(Level::Warn, tag, msg);
}
pub fn error(tag: &str, msg: &str) {
    log(Level::Error, tag, msg);
}
