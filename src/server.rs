use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;

/// The webapp page, embedded so the deployed binary stays one file. v1 serves the
/// static mockup; W3/W4 add the /api/* endpoints behind the same listener.
const PAGE: &str = include_str!("webapp/mockup.html");

/// Deterministic auto-port per the webapp spec: hash the project's absolute path
/// into 9700–9899 so the same project gets the same port every restart (stable
/// insert targets for agents and scripts), probing upward on a clash. An explicit
/// `want` port is bound exactly or fails loudly.
pub fn bind(project_root: &Path, want: Option<u16>) -> std::io::Result<(TcpListener, u16)> {
    if let Some(port) = want {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        return Ok((listener, port));
    }
    let canon = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let mut hash: u32 = 0;
    for byte in canon.to_string_lossy().bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u32);
    }
    for offset in 0..200u32 {
        let port = 9700 + (((hash % 200) + offset) % 200) as u16;
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            return Ok((listener, port));
        }
    }
    // Whole range busy (200 running engines?) — let the OS pick.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// URL slug: `url_slug` from .iter/projects.json when present, else the project
/// directory name sanitized to [a-z0-9-].
pub fn slug(project_root: &Path) -> String {
    let settings = project_root.join(".iter").join("projects.json");
    if let Ok(text) = std::fs::read_to_string(&settings) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(s) = v.get("url_slug").and_then(|s| s.as_str()) {
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    let name = project_root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".into());
    let cleaned: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    cleaned.trim_matches('-').to_string()
}

/// Serve the embedded page on a background thread. Loopback-only by construction
/// (we bind 127.0.0.1); every request gets the page — routing arrives with the API.
pub fn spawn(listener: TcpListener) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                PAGE.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(PAGE.as_bytes());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_port_is_deterministic_and_in_range() {
        let dir = std::env::temp_dir().join(format!("iter-port-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (l1, p1) = bind(&dir, None).unwrap();
        assert!((9700..9900).contains(&p1), "port {} outside range", p1);
        drop(l1);
        let (_l2, p2) = bind(&dir, None).unwrap();
        assert_eq!(p1, p2, "same project must hash to the same port");
        // Occupied port probes to a neighbor instead of failing.
        let (_l3, p3) = bind(&dir, None).unwrap();
        assert_ne!(p2, p3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slug_from_dirname_and_settings() {
        let dir = std::env::temp_dir().join(format!("My Project_{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        let s = slug(&dir);
        assert!(s.starts_with("my-project-"), "sanitized dirname, got {}", s);
        std::fs::write(dir.join(".iter/projects.json"), r#"{"url_slug":"pdy-dev"}"#).unwrap();
        assert_eq!(slug(&dir), "pdy-dev");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
