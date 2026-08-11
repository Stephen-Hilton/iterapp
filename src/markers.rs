use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Marker discovery: any file matching the marker glob (default `**/*.iter.md`) is
/// iterapp-meaningful; its FRONTMATTER declares its role — `level:` makes a structure
/// node, `interface:` makes a globally-aggregated interface definition,
/// `participants:` makes a use-case thread, none of those makes a plain context doc.

#[derive(Debug, Clone, Serialize, Default)]
pub struct Node {
    pub key: String,   // dir of the marker, relative to the project root
    pub name: String,
    pub level: String,
    pub description: String,
    pub parent: String, // explicit frontmatter override; "" = derive by directory
    pub uses: Vec<String>,     // interface ids or plain resource names (consumer end)
    pub provides: Vec<String>, // interface ids this node serves (provider end)
    pub dir: String,  // absolute directory (codepath for Create-WorkItem)
    pub path: String, // absolute marker file path
    pub depth: usize,
}

/// Interfaces are contracts BETWEEN nodes, so they aggregate globally (the file may
/// live anywhere) and are referenced from nodes via uses:/provides: — never owned by
/// the hierarchy. Duplicate ids across files are surfaced for rationalization.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Interface {
    pub id: String,
    pub kind: String,     // http|grpc|kafka|sql|file|cli|library|… (free-form)
    pub endpoint: String, // machine-usable address, when the kind has one
    pub description: String,
    pub file: String, // absolute marker file path (the contract body lives here)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UseCase {
    pub name: String,
    pub description: String,
    pub file: String,
    /// node key → step label ("2.1")
    pub steps: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Scan {
    pub nodes: Vec<Node>,
    pub interfaces: Vec<Interface>,
    pub usecases: Vec<UseCase>,
    pub plain: Vec<String>,
    pub roots: Vec<String>,
}

/// Flat `key: value` frontmatter between `---` fences; returns (map, participant lines).
fn frontmatter(content: &str) -> (HashMap<String, String>, Vec<String>) {
    let mut map = HashMap::new();
    let mut participants = Vec::new();
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else { return (map, participants) };
    let Some(end) = rest.find("\n---") else { return (map, participants) };
    let mut in_participants = false;
    for line in rest[..end].lines() {
        let raw = line.trim_end();
        let t = raw.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if in_participants && t.starts_with("- ") {
            participants.push(t[2..].trim().to_string());
            continue;
        }
        in_participants = false;
        if let Some((key, val)) = t.split_once(':') {
            let key = key.trim().to_string();
            let val = val.trim().trim_matches('"').trim_matches('\'').to_string();
            if key == "participants" && val.is_empty() {
                in_participants = true;
            }
            map.insert(key, val);
        }
    }
    (map, participants)
}

fn parse_list(val: &str) -> Vec<String> {
    val.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Scan the configured roots for markers and sort them into roles.
pub fn scan(project_root: &Path, roots: &[PathBuf], marker_glob: &str) -> Scan {
    let mut result = Scan::default();
    let project_root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    for root in roots {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        result.roots.push(root.to_string_lossy().into_owned());
        let pattern = root.join(marker_glob);
        let Ok(paths) = glob::glob(&pattern.to_string_lossy()) else { continue };
        for path in paths.flatten() {
            if !path.is_file() || path.components().any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "target" | "node_modules"))) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            let (front, participants) = frontmatter(&content);
            let rel_dir = path
                .parent()
                .and_then(|d| d.strip_prefix(&project_root).ok().or_else(|| d.strip_prefix(&root).ok()))
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default();
            if front.contains_key("level") {
                let dir_abs = path.parent().unwrap_or(&root).to_string_lossy().into_owned();
                let name = front.get("name").cloned().unwrap_or_else(|| {
                    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
                });
                result.nodes.push(Node {
                    depth: if rel_dir.is_empty() { 0 } else { rel_dir.split('/').count() },
                    key: rel_dir.clone(),
                    name,
                    level: front.get("level").cloned().unwrap_or_default(),
                    description: front.get("description").cloned().unwrap_or_default(),
                    parent: front.get("parent").cloned().unwrap_or_default(),
                    uses: front.get("uses").map(|v| parse_list(v)).unwrap_or_default(),
                    provides: front.get("provides").map(|v| parse_list(v)).unwrap_or_default(),
                    dir: dir_abs,
                    path: path.to_string_lossy().into_owned(),
                });
            } else if front.contains_key("interface") {
                result.interfaces.push(Interface {
                    id: front.get("interface").cloned().unwrap_or_default(),
                    kind: front.get("kind").cloned().unwrap_or_default(),
                    endpoint: front.get("endpoint").cloned().unwrap_or_default(),
                    description: front.get("description").cloned().unwrap_or_default(),
                    file: path.to_string_lossy().into_owned(),
                });
            } else if !participants.is_empty() {
                // participant line: "<step> <node key>", e.g. "2.1 core/intake"
                let mut steps = HashMap::new();
                for p in &participants {
                    if let Some((step, key)) = p.split_once(char::is_whitespace) {
                        steps.insert(key.trim().to_string(), step.trim().to_string());
                    }
                }
                result.usecases.push(UseCase {
                    name: front.get("name").cloned().unwrap_or_default(),
                    description: front.get("description").cloned().unwrap_or_default(),
                    file: path.to_string_lossy().into_owned(),
                    steps,
                });
            } else {
                result.plain.push(path.to_string_lossy().into_owned());
            }
        }
    }
    // Hierarchy order: sort nodes by key so directory nesting reads as a tree.
    result.nodes.sort_by(|a, b| a.key.cmp(&b.key));
    result.interfaces.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-markers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("core/intake")).unwrap();
        dir
    }

    #[test]
    fn roles_sorted_by_frontmatter() {
        let dir = tmpdir();
        std::fs::write(
            dir.join("root.iter.md"),
            "---\nname: My Project\nlevel: project\ndescription: \"top\"\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/intake/intake.iter.md"),
            "---\nname: Intake\nlevel: container\nuses: [postgres, kafka]\n---\ncontext body",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/payout.iter.md"),
            "---\nname: Early payout\nparticipants:\n  - 1 core\n  - 2.1 core/intake\n---\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/evidence-api.iter.md"),
            "---\ninterface: evidence-api\nkind: http\nendpoint: POST /v1/evidence\ndescription: \"store artifacts\"\n---\ncontract body",
        )
        .unwrap();
        std::fs::write(dir.join("core/bizreq.iter.md"), "no frontmatter, plain context\n").unwrap();

        let scan = scan(&dir, &[dir.clone()], "**/*.iter.md");
        assert_eq!(scan.nodes.len(), 2, "level: files are nodes");
        assert_eq!(scan.interfaces.len(), 1, "interface: files aggregate globally");
        assert_eq!(scan.interfaces[0].id, "evidence-api");
        assert_eq!(scan.interfaces[0].kind, "http");
        assert_eq!(scan.nodes[0].key, "");
        assert_eq!(scan.nodes[0].name, "My Project");
        assert_eq!(scan.nodes[1].key, "core/intake");
        assert_eq!(scan.nodes[1].uses, vec!["postgres", "kafka"]);
        assert_eq!(scan.nodes[1].depth, 2);
        assert_eq!(scan.usecases.len(), 1, "participants: files are use-cases");
        assert_eq!(scan.usecases[0].steps.get("core/intake").unwrap(), "2.1");
        assert_eq!(scan.plain.len(), 1, "frontmatter-less files are plain context");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
