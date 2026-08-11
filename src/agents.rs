use std::path::Path;

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub type_name: String,
    pub description: String,
    pub visible: bool,
    pub max_agent_count: usize,
    pub max_work_timeout_sec: u64,
    pub max_connection_timeout_sec: u64,
    pub model: String,
    pub model_flags: String,
    pub llm_run_mode: String,
    pub sleep_interval_sec: u64,
    pub body: String,
}

impl Default for AgentDef {
    fn default() -> Self {
        AgentDef {
            type_name: String::new(),
            description: String::new(),
            visible: true,
            max_agent_count: 1,
            max_work_timeout_sec: 3600,
            max_connection_timeout_sec: 30,
            model: "opus".into(),
            model_flags: String::new(),
            llm_run_mode: "headless".into(),
            sleep_interval_sec: 30,
            body: String::new(),
        }
    }
}

pub fn agents_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".iter").join("agents")
}

/// Discover all agent definitions in `.iter/agents/*.md`. Files that are empty or
/// unreadable are skipped with a warning on stderr.
pub fn discover(project_root: &Path) -> Vec<AgentDef> {
    let dir = agents_dir(project_root);
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = match path.file_stem().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => out.push(parse(&name, &text)),
            Ok(_) => eprintln!("warning: agent file {} is empty; skipped", path.display()),
            Err(e) => eprintln!("warning: cannot read {}: {}", path.display(), e),
        }
    }
    out.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    out
}

/// Parse an agent markdown file: flat `key: value` YAML frontmatter between `---`
/// fences, followed by the prompt body. Unknown keys are ignored.
pub fn parse(type_name: &str, content: &str) -> AgentDef {
    let mut def = AgentDef { type_name: type_name.to_string(), ..Default::default() };
    let (front, body) = split_frontmatter(content);
    def.body = body.trim().to_string();
    for line in front.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once(':') else { continue };
        let val = unquote(raw.trim());
        match key.trim() {
            "description" => def.description = val,
            "visible" => def.visible = val == "true",
            "max_agent_count" => def.max_agent_count = val.parse().unwrap_or(def.max_agent_count),
            "max_work_timeout_sec" => def.max_work_timeout_sec = val.parse().unwrap_or(def.max_work_timeout_sec),
            "max_connection_timeout_sec" => def.max_connection_timeout_sec = val.parse().unwrap_or(def.max_connection_timeout_sec),
            "model" => def.model = val,
            "model_flags" => def.model_flags = val,
            "llm_run_mode" => def.llm_run_mode = val,
            "sleep_interval_sec" => def.sleep_interval_sec = val.parse().unwrap_or(def.sleep_interval_sec),
            _ => {}
        }
    }
    def
}

fn split_frontmatter(content: &str) -> (String, String) {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let front = rest[..end].to_string();
            let body = rest[end + 4..].to_string();
            return (front, body);
        }
    }
    (String::new(), content.to_string())
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_template_code_agent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let agents = discover(&root);
        let code = agents.iter().find(|a| a.type_name == "code").expect("code agent");
        assert_eq!(code.max_agent_count, 3);
        assert_eq!(code.model, "opus");
        assert_eq!(code.model_flags, "--dangerously-skip-permissions");
        assert_eq!(code.llm_run_mode, "headless");
        assert!(code.body.contains("code"));
        assert!(agents.len() >= 6, "expected 6 template agents, got {}", agents.len());
    }

    #[test]
    fn frontmatter_edge_cases() {
        let def = parse("x", "---\ndescription: 'single quoted'\nvisible: false\nmax_agent_count: 7\nunknown_key: whatever\n---\nBody text");
        assert_eq!(def.description, "single quoted");
        assert!(!def.visible);
        assert_eq!(def.max_agent_count, 7);
        assert_eq!(def.body, "Body text");
    }

    #[test]
    fn no_frontmatter_is_all_body() {
        let def = parse("x", "just a body");
        assert_eq!(def.body, "just a body");
        assert_eq!(def.max_agent_count, 1);
    }
}
