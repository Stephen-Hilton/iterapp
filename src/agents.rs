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
    /// New-WorkItem form defaults (2026-08-18): when this agent type is
    /// selected, the form pre-fills codepath/codepath_ignore from these — so
    /// users don't have to remember conventions like the usecase agent's
    /// usecases-dir scope. `default_codepath` may use `{usecase_dir}` /
    /// `{interface_dir}` / `{test_dir}` placeholders, resolved by the server
    /// per project; `default_codepath_ignore` is comma-separated patterns.
    /// Empty = no opinion, the form leaves the field alone.
    pub default_codepath: String,
    pub default_codepath_ignore: String,
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
            default_codepath: String::new(),
            default_codepath_ignore: String::new(),
            body: String::new(),
        }
    }
}

pub fn agents_dir(project_root: &Path) -> std::path::PathBuf {
    project_root.join(".iter").join("agents")
}

/// Discover all agent definitions in `.iter/agents/*.md`. Files whose name starts
/// with `_` are helpers, not agent types (e.g. `_shared.md`); files that are empty
/// or unreadable are skipped with a warning on stderr.
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
        if name.starts_with('_') {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => out.push(parse(&name, &text)),
            Ok(_) => eprintln!("warning: agent file {} is empty; skipped", path.display()),
            Err(e) => eprintln!("warning: cannot read {}: {}", path.display(), e),
        }
    }
    out.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    out
}

/// Instructions shared by EVERY agent: `.iter/agents/_shared.md`. Appended to each
/// agent's composed context at run time — the store-once place for all-agent rules.
/// Returns None when the file is missing or effectively empty.
pub fn shared_instructions(project_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(agents_dir(project_root).join("_shared.md")).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
            "default_codepath" => def.default_codepath = val,
            "default_codepath_ignore" => def.default_codepath_ignore = val,
            _ => {}
        }
    }
    def
}

/// Frontmatter keys the settings editor may update — the same set `parse` reads.
pub const EDITABLE_KEYS: &[&str] = &[
    "description",
    "visible",
    "max_agent_count",
    "max_work_timeout_sec",
    "max_connection_timeout_sec",
    "model",
    "model_flags",
    "llm_run_mode",
    "sleep_interval_sec",
    "default_codepath",
    "default_codepath_ignore",
];

/// Rewrite an agent file's text with `updates` applied to its frontmatter and the
/// body replaced when `new_body` is given. Frontmatter lines the editor doesn't
/// know about (unknown keys, comments) are kept in place; updated keys keep their
/// position; keys not present yet are appended before the closing fence. A file
/// with no frontmatter gains one.
pub fn apply_updates(content: &str, updates: &[(String, String)], new_body: Option<&str>) -> String {
    let (front, body) = split_frontmatter(content);
    let mut remaining: Vec<&(String, String)> = updates.iter().collect();
    let mut lines: Vec<String> = Vec::new();
    for line in front.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.split_once(':').map(|(k, _)| k.trim()).unwrap_or("");
        if let Some(pos) = remaining.iter().position(|(k, _)| k == key) {
            let (k, v) = remaining.remove(pos);
            lines.push(format!("{}: {}", k, quote_if_needed(v)));
        } else {
            lines.push(line.to_string());
        }
    }
    for (k, v) in remaining {
        lines.push(format!("{}: {}", k, quote_if_needed(v)));
    }
    let body = new_body.unwrap_or(&body);
    format!("---\n{}\n---\n\n{}\n", lines.join("\n"), body.trim())
}

/// Bare values round-trip through `parse` except when they'd be misread: empty,
/// whitespace-padded, comment-like, or already quote-wrapped. `unquote` only strips
/// a matching outer pair, so inner quotes survive.
fn quote_if_needed(v: &str) -> String {
    let needs = v.is_empty()
        || v != v.trim()
        || v.starts_with('#')
        || (v.starts_with('"') && v.ends_with('"'))
        || (v.starts_with('\'') && v.ends_with('\''));
    if needs {
        format!("\"{}\"", v)
    } else {
        v.to_string()
    }
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
        assert!(code.default_codepath.is_empty(), "code has no codepath opinion");
        let usecase = agents.iter().find(|a| a.type_name == "usecase").expect("usecase agent");
        assert_eq!(usecase.default_codepath, "{usecase_dir}");
        assert_eq!(usecase.default_codepath_ignore, "{test_dir}/");
        assert!(agents.len() >= 5, "expected 5 template agents, got {}", agents.len());
        assert!(
            !agents.iter().any(|a| a.type_name == "test"),
            "the test agent is retired — the deterministic sweep runs tests now"
        );
        assert!(
            !agents.iter().any(|a| a.type_name.starts_with('_')),
            "underscore-prefixed helper files must not become agent types"
        );
    }

    #[test]
    fn shared_instructions_loaded_from_underscore_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let text = shared_instructions(&root).expect("template ships _shared.md");
        assert!(text.contains("frontmatter"), "the template's first shared rule");
        assert!(shared_instructions(Path::new("/nonexistent/nowhere")).is_none());
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

    #[test]
    fn apply_updates_edits_in_place_and_appends_new_keys() {
        let original = "---\n# tuning\ndescription: \"old\"\nmax_agent_count: 3\ncustom_key: kept\n---\n\nBody stays\n";
        let updates = vec![
            ("description".to_string(), "new words".to_string()),
            ("model".to_string(), "sonnet".to_string()),
        ];
        let out = apply_updates(original, &updates, None);
        let def = parse("x", &out);
        assert_eq!(def.description, "new words");
        assert_eq!(def.model, "sonnet");
        assert_eq!(def.max_agent_count, 3, "untouched keys keep their values");
        assert!(out.contains("# tuning"), "comments survive");
        assert!(out.contains("custom_key: kept"), "unknown keys survive");
        assert_eq!(def.body, "Body stays");
        let front = out.split("---").nth(1).unwrap();
        assert!(
            front.find("description").unwrap() < front.find("max_agent_count").unwrap(),
            "updated keys keep their original position"
        );
    }

    #[test]
    fn apply_updates_replaces_body_and_creates_frontmatter() {
        let out = apply_updates("no frontmatter here", &[("model".to_string(), "opus".to_string())], Some("new body"));
        let def = parse("x", &out);
        assert_eq!(def.model, "opus");
        assert_eq!(def.body, "new body");
    }

    #[test]
    fn apply_updates_round_trips_awkward_values() {
        for val in ["", "  padded  ", "# looks like a comment", "\"already quoted\"", "colons: are: fine", "--flag-with-dashes"] {
            let out = apply_updates("---\n---\nb", &[("description".to_string(), val.to_string())], None);
            assert_eq!(parse("x", &out).description, val, "value {:?} must round-trip", val);
        }
    }
}
