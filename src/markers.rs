use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Marker discovery: any file matching the marker glob (default `**/*.iter.md`) is
/// iterapp-meaningful, and its FILENAME declares what it IS (decided 2026-08-16):
/// the characters right before `.iter.md` are the role. Frontmatter supplies
/// attributes, never identity — a stray `level:` key inside a usecase file changes
/// nothing; the only way to turn a use-case into a structure node is to RENAME the
/// file to `…marker.iter.md`.
///
/// Roles (all singular): `marker` (structure node / C4 object), `bizreq`,
/// `techreq` (requirement docs), `interface` (contract), `testgroup` (test
/// definitions), `usecase` (thread). Any prefix works: `usecase.iter.md`,
/// `my_new.usecase.iter.md`, `pdy_core_ingest_v2usecase.iter.md` are all
/// use-cases. An `.iter.md` file whose name ends in none of the roles is a plain
/// context doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Marker,
    Bizreq,
    Techreq,
    Interface,
    Testgroup,
    Usecase,
}

const ROLES: &[(Role, &str)] = &[
    (Role::Marker, "marker"),
    (Role::Bizreq, "bizreq"),
    (Role::Techreq, "techreq"),
    (Role::Interface, "interface"),
    (Role::Testgroup, "testgroup"),
    (Role::Usecase, "usecase"),
];

/// The role a filename declares, from its suffix right before `.iter.md`.
/// At most one role can match (no role name is a suffix of another), so
/// `techreq.usecase.iter.md` is a usecase — the role adjacent to `.iter.md` wins.
pub fn role_of(filename: &str) -> Option<Role> {
    let lower = filename.to_ascii_lowercase();
    if !lower.ends_with(".iter.md") {
        return None;
    }
    ROLES
        .iter()
        .find(|(_, tag)| lower.ends_with(&format!("{}.iter.md", tag)))
        .map(|(role, _)| *role)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Node {
    pub key: String,   // dir of the marker, relative to the project root
    pub name: String,
    pub level: String,
    pub description: String,
    pub parent: String, // explicit frontmatter override; "" = derive by directory
    pub uses: Vec<String>,     // interface ids or plain resource names (consumer end)
    pub provides: Vec<String>, // interface ids this node serves (provider end)
    /// Project-wide reqs dir override — meaningful on the project-level marker only
    /// (see context::reqs_dir). "" = default `.iter/reqs/`.
    pub reqs: String,
    /// This C4 object's testgroup.iter.md, relative to the marker file's directory
    /// (`testgroup:` frontmatter key). MANDATORY for the test sweep: the marker file
    /// defines the C4 object, so test ownership is declared here, never inferred
    /// from file positions. No key = this C4 object is deliberately outside the sweep.
    pub testgroup: String,
    /// Subtree (relative to the marker file's directory) that holds test scripts —
    /// the testwriter's lock scope, carved out of code items via codepath_ignore.
    pub test_dir: String,
    /// Test-Loop gate (`test_loop:` key — features/test_loop_flag.md):
    /// "omit" parks this object (and its subtree) out of the sweep, "include"
    /// re-enters a subtree under an omitted ancestor, "blocked" is the hard
    /// form (vendor/outside setup missing) that no descendant include and no
    /// `iter testloop --include` can override. Empty = no opinion (inherit).
    pub test_loop: String,
    /// This C4 object's local requirement files, relative to the marker file's
    /// directory. Optional; the webapp falls back to `bizreq.iter.md` /
    /// `techreq.iter.md` beside the marker when absent.
    pub bizreq: String,
    pub techreq: String,
    pub dir: String,  // absolute directory (codepath for Create-WorkItem)
    pub path: String, // absolute marker file path
    pub depth: usize,
    pub body: String, // the marker body — the context document itself
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
    pub file: String, // absolute marker file path
    pub body: String, // the contract itself — often a long multi-line document
    /// Contract-enforcement tests (2026-08-17): `testgroup:` names this
    /// interface's testgroup.iter.md (relative to this file) whose scripts
    /// assert the REAL providers' I/O against the contract's example — drift
    /// turns red instead of silently accumulating. Empty = a coverage gap the
    /// sweep turns into a testwriter authoring item; the literal value `none`
    /// opts out deliberately.
    pub testgroup: String,
    pub test_dir: String,
    /// Test-Loop gate, own-flag only (interfaces are global objects the
    /// hierarchy doesn't own — no ancestry): "omit" or "blocked" parks the
    /// contract tests; empty (or anything else) = swept as usual.
    pub test_loop: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UseCase {
    pub name: String,
    pub description: String,
    pub file: String,
    /// node key → step label ("2.1")
    pub steps: HashMap<String, String>,
    /// The raw ordered participant lines ("2.1 core/intake") — steps loses order,
    /// and the UI's CRUD editor round-trips these verbatim.
    pub participants: Vec<String>,
    /// The narrative after the frontmatter — the use-case description itself.
    pub body: String,
    /// End-to-end journey tests (2026-08-17): `testgroup:` names this
    /// use-case's testgroup.iter.md (relative to this file) whose scripts walk
    /// the actual user journey — user-centric TDD's steering signal. Empty = a
    /// coverage gap the sweep turns into a testwriter authoring item; the
    /// literal value `none` opts out deliberately.
    pub testgroup: String,
    pub test_dir: String,
    /// Test-Loop gate, own-flag only (like interfaces): "omit" parks a
    /// not-yet-in-focus use case, "blocked" the hard form. Empty = swept.
    pub test_loop: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Scan {
    pub nodes: Vec<Node>,
    pub interfaces: Vec<Interface>,
    pub usecases: Vec<UseCase>,
    pub plain: Vec<String>,
    pub roots: Vec<String>,
}

/// Body cap per marker in scan responses: contracts can be long multi-line JSON
/// documents and the UI shows them in full, but a runaway file shouldn't bloat the API.
const BODY_CAP: usize = 65536;

/// Flat `key: value` frontmatter between `---` fences; returns
/// (map, participant lines, body — everything after the closing fence).
pub(crate) fn frontmatter(content: &str) -> (HashMap<String, String>, Vec<String>, String) {
    let mut map = HashMap::new();
    let mut participants = Vec::new();
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else { return (map, participants, String::new()) };
    let Some(end) = rest.find("\n---") else { return (map, participants, String::new()) };
    let mut body = rest[end + 4..].trim_start_matches('-').trim().to_string();
    if body.len() > BODY_CAP {
        body.truncate(BODY_CAP);
        body.push_str("\n… (truncated — read the full file on disk)");
    }
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
            // YAML comment rule: an unquoted value ends at the first ` #`;
            // quoted values keep their `#` characters.
            let raw = val.trim();
            let val = if raw.starts_with('"') || raw.starts_with('\'') {
                raw.trim_matches('"').trim_matches('\'').to_string()
            } else {
                match raw.find(" #") {
                    Some(i) => raw[..i].trim_end().to_string(),
                    None => raw.to_string(),
                }
            };
            if key == "participants" && val.is_empty() {
                in_participants = true;
            }
            map.insert(key, val);
        }
    }
    (map, participants, body)
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
            // The FILENAME declares the role (see role_of); frontmatter only
            // supplies attributes. A stray `level:` in a usecase/testgroup file
            // changes nothing — renaming the file is the only way to change role.
            let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let (front, participants, body) = frontmatter(&content);
            let rel_dir = path
                .parent()
                .and_then(|d| d.strip_prefix(&project_root).ok().or_else(|| d.strip_prefix(&root).ok()))
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default();
            match role_of(&fname) {
                Some(Role::Marker) => {
                    let dir_abs = path.parent().unwrap_or(&root).to_string_lossy().into_owned();
                    let name = front.get("name").cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| {
                        // Default name: the filename minus the role suffix
                        // ("pdy_core_x.marker.iter.md" → "pdy_core_x"), else the dir.
                        let stem = fname.trim_end_matches(".iter.md").trim_end_matches("marker");
                        let stem = stem.trim_end_matches(['.', '-', '_']);
                        if stem.is_empty() {
                            path.parent()
                                .and_then(|d| d.file_name())
                                .map(|d| d.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        } else {
                            stem.to_string()
                        }
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
                        reqs: front.get("reqs").cloned().unwrap_or_default(),
                        testgroup: front.get("testgroup").cloned().unwrap_or_default(),
                        test_dir: front.get("test_dir").cloned().unwrap_or_default(),
                        test_loop: front.get("test_loop").cloned().unwrap_or_default(),
                        bizreq: front.get("bizreq").cloned().unwrap_or_default(),
                        techreq: front.get("techreq").cloned().unwrap_or_default(),
                        dir: dir_abs,
                        path: path.to_string_lossy().into_owned(),
                        body,
                    });
                }
                Some(Role::Interface) => {
                    result.interfaces.push(Interface {
                        id: front.get("interface").cloned().unwrap_or_default(),
                        kind: front.get("kind").cloned().unwrap_or_default(),
                        endpoint: front.get("endpoint").cloned().unwrap_or_default(),
                        description: front.get("description").cloned().unwrap_or_default(),
                        file: path.to_string_lossy().into_owned(),
                        body,
                        testgroup: front.get("testgroup").cloned().unwrap_or_default(),
                        test_dir: front.get("test_dir").cloned().unwrap_or_default(),
                        test_loop: front.get("test_loop").cloned().unwrap_or_default(),
                    });
                }
                Some(Role::Usecase) => {
                    // participant line: "<step> <node key>", e.g. "2.1 core/intake";
                    // "." refers to the project-root node (whose key is "").
                    let mut steps = HashMap::new();
                    for p in &participants {
                        if let Some((step, key)) = p.split_once(char::is_whitespace) {
                            let key = key.trim();
                            let key = if key == "." { "" } else { key };
                            steps.insert(key.to_string(), step.trim().to_string());
                        }
                    }
                    result.usecases.push(UseCase {
                        name: front.get("name").cloned().unwrap_or_default(),
                        description: front.get("description").cloned().unwrap_or_default(),
                        file: path.to_string_lossy().into_owned(),
                        steps,
                        participants: participants.clone(),
                        body,
                        testgroup: front.get("testgroup").cloned().unwrap_or_default(),
                        test_dir: front.get("test_dir").cloned().unwrap_or_default(),
                        test_loop: front.get("test_loop").cloned().unwrap_or_default(),
                    });
                }
                // bizreq/techreq/testgroup files and unrecognized suffixes are plain
                // context docs to the structure view (testgroup files are read by the
                // test engine through the owning marker's `testgroup:` key).
                Some(Role::Bizreq) | Some(Role::Techreq) | Some(Role::Testgroup) | None => {
                    result.plain.push(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    // Hierarchy order: sort nodes by key so directory nesting reads as a tree.
    result.nodes.sort_by(|a, b| a.key.cmp(&b.key));
    result.interfaces.sort_by(|a, b| a.id.cmp(&b.id));
    result
}

/* --------------------------- Test-Loop gate (features/test_loop_flag.md) ---
The `test_loop:` frontmatter flag parks objects out of the deterministic test
sweep without removing anything: "omit" (workflow parking, agents may flip it
back with `iter testloop --include`), "include" (re-enter a subtree under an
omitted ancestor), "blocked" (hard park — vendor/outside setup missing; beats
every descendant include, and only a human editing the file lifts it). */

pub const TEST_LOOP_OMIT: &str = "omit";
pub const TEST_LOOP_INCLUDE: &str = "include";
pub const TEST_LOOP_BLOCKED: &str = "blocked";

/// The effective Test-Loop state of an object after ancestry resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum TestLoopState {
    Included,
    /// value = "omit" | "blocked"; by = key (or file) of the deciding marker.
    Omitted { value: String, by: String },
}

/// Is `anc` the same node as, or a directory ancestor of, the node at `key`?
/// Node keys are dirs relative to the project root; the root node's key is "".
fn is_self_or_ancestor(anc: &str, key: &str) -> bool {
    anc == key || anc.is_empty() || key.starts_with(&format!("{}/", anc))
}

fn display_key(key: &str) -> String {
    if key.is_empty() { ".".to_string() } else { key.to_string() }
}

/// Effective Test-Loop state of a C4 node. `blocked` anywhere on the chain
/// (self included) wins — out, not overridable below; otherwise the NEAREST
/// explicit omit/include walking up from the node decides; no flag anywhere =
/// included. Ancestry is directory nesting, the same derivation the Projects
/// tree renders.
pub fn effective_test_loop(node: &Node, nodes: &[Node]) -> TestLoopState {
    let mut chain: Vec<&Node> =
        nodes.iter().filter(|n| is_self_or_ancestor(&n.key, &node.key)).collect();
    chain.sort_by_key(|n| std::cmp::Reverse(n.key.len())); // nearest (deepest) first
    if let Some(b) = chain.iter().find(|n| n.test_loop.trim() == TEST_LOOP_BLOCKED) {
        return TestLoopState::Omitted { value: TEST_LOOP_BLOCKED.into(), by: display_key(&b.key) };
    }
    for n in &chain {
        match n.test_loop.trim() {
            TEST_LOOP_OMIT => {
                return TestLoopState::Omitted { value: TEST_LOOP_OMIT.into(), by: display_key(&n.key) }
            }
            TEST_LOOP_INCLUDE => return TestLoopState::Included,
            _ => {}
        }
    }
    TestLoopState::Included
}

/// Test-Loop state of a use-case or interface: own flag only — global objects
/// the hierarchy doesn't own get no ancestry.
pub fn own_test_loop(test_loop: &str, file: &str) -> TestLoopState {
    match test_loop.trim() {
        v @ (TEST_LOOP_OMIT | TEST_LOOP_BLOCKED) => {
            TestLoopState::Omitted { value: v.into(), by: file.to_string() }
        }
        _ => TestLoopState::Included,
    }
}

/// Deterministically set (Some) or remove (None) one scalar frontmatter key,
/// preserving every other line verbatim — a record-level edit like the usecase
/// participants editor. The file must already have a frontmatter fence.
pub fn set_frontmatter_key(path: &Path, key: &str, value: Option<&str>) -> Result<(), String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return Err(format!("{} has no frontmatter fence", path.display()));
    };
    let Some(end) = rest.find("\n---") else {
        return Err(format!("{} has no closing frontmatter fence", path.display()));
    };
    let mut kept: Vec<String> = Vec::new();
    let prefix = format!("{}:", key);
    for line in rest[..end].lines() {
        if line.trim_start().starts_with(&prefix) {
            continue;
        }
        kept.push(line.to_string());
    }
    if let Some(v) = value {
        kept.push(format!("{}: {}", key, v));
    }
    let new_content = format!("---{}\n---{}", kept.join("\n"), &rest[end + 4..]);
    std::fs::write(path, new_content).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

/// A `test_loop` edit action, shared by `iter testloop` and POST /api/testloop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestLoopAction {
    Omit,
    Include,
    Block,
    Clear,
}

/// One resolvable Test-Loop target: a node, use-case, or interface.
struct TestLoopTarget {
    label: String, // what messages call it
    path: String,  // the file the flag lives in
    current: String,
    node_key: Option<String>,
}

/// Resolve `target` (node key, node name, use-case name, interface id, or a
/// path suffix of the declaring file) against the scan — exactly one match or
/// refusal, never a guess.
fn resolve_test_loop_target(scan: &Scan, target: &str) -> Result<TestLoopTarget, String> {
    let t = target.trim().trim_end_matches('/');
    let mut matches: Vec<TestLoopTarget> = Vec::new();
    for n in &scan.nodes {
        if n.key == t || n.name == t || (t == "." && n.key.is_empty()) || n.path.ends_with(t) {
            matches.push(TestLoopTarget {
                label: format!("object {} ({})", n.name, display_key(&n.key)),
                path: n.path.clone(),
                current: n.test_loop.trim().to_string(),
                node_key: Some(n.key.clone()),
            });
        }
    }
    for u in &scan.usecases {
        if u.name == t || u.file.ends_with(t) {
            matches.push(TestLoopTarget {
                label: format!("use case {}", u.name),
                path: u.file.clone(),
                current: u.test_loop.trim().to_string(),
                node_key: None,
            });
        }
    }
    for i in &scan.interfaces {
        if i.id == t || i.file.ends_with(t) {
            matches.push(TestLoopTarget {
                label: format!("interface {}", i.id),
                path: i.file.clone(),
                current: i.test_loop.trim().to_string(),
                node_key: None,
            });
        }
    }
    matches.dedup_by(|a, b| a.path == b.path);
    match matches.len() {
        0 => Err(format!("\"{}\" matches no C4 object, use case, or interface", target)),
        1 => Ok(matches.pop().expect("one match")),
        n => Err(format!(
            "\"{}\" is ambiguous ({} matches: {}) — use the node key or file path",
            target,
            n,
            matches.iter().map(|m| m.label.as_str()).collect::<Vec<_>>().join("; ")
        )),
    }
}

/// Apply one Test-Loop edit. Refusals guard the `blocked` contract: an
/// existing `blocked` value survives everything except a human editing the
/// marker file (so an agent told to include its dependencies can never flip a
/// vendor-blocked object — it gets a refusal to report instead), and
/// `--include` under a blocked ANCESTOR refuses too, since the include could
/// never take effect. Returns a one-line summary of what changed.
pub fn testloop_apply(scan: &Scan, target: &str, action: TestLoopAction) -> Result<String, String> {
    let t = resolve_test_loop_target(scan, target)?;
    if t.current == TEST_LOOP_BLOCKED && action != TestLoopAction::Block {
        return Err(format!(
            "{} is test_loop: blocked (outside/vendor setup missing) — refusing to change it; \
             a human lifts a block by editing {}",
            t.label, t.path
        ));
    }
    if action == TestLoopAction::Include {
        if let Some(key) = &t.node_key {
            let blocked_anc = scan.nodes.iter().find(|n| {
                n.key != *key
                    && is_self_or_ancestor(&n.key, key)
                    && n.test_loop.trim() == TEST_LOOP_BLOCKED
            });
            if let Some(anc) = blocked_anc {
                return Err(format!(
                    "{} sits under blocked ancestor {} (test_loop: blocked) — an include there \
                     can never take effect; a human lifts the block by editing {}",
                    t.label,
                    display_key(&anc.key),
                    anc.path
                ));
            }
        }
    }
    let value = match action {
        TestLoopAction::Omit => Some(TEST_LOOP_OMIT),
        TestLoopAction::Include => Some(TEST_LOOP_INCLUDE),
        TestLoopAction::Block => Some(TEST_LOOP_BLOCKED),
        TestLoopAction::Clear => None,
    };
    set_frontmatter_key(Path::new(&t.path), "test_loop", value)?;
    Ok(format!(
        "{}: test_loop {} (was {})",
        t.label,
        value.map(|v| format!("→ {}", v)).unwrap_or_else(|| "cleared".into()),
        if t.current.is_empty() { "unset" } else { &t.current }
    ))
}

/// Does this marker text already carry a `# Long Description` heading (any level)?
fn has_long_description(content: &str) -> bool {
    content.lines().any(|l| {
        let t = l.trim_start();
        let hashes = t.chars().take_while(|c| *c == '#').count();
        (1..=3).contains(&hashes) && t[hashes..].trim_start().to_lowercase().starts_with("long description")
    })
}

/// One-time maintenance sweep (`iter stubdesc`): append `# Long Description\nTBD`
/// to every structure-node marker that lacks the section, so a follow-up plan item
/// can target `contains "Long Description: TBD"` and fill them in. Returns the
/// stubbed file paths. New markers should never need this — agents write real long
/// descriptions at creation (see _shared.md).
pub fn stub_long_descriptions(scan: &Scan) -> Vec<String> {
    let mut stubbed = Vec::new();
    for node in &scan.nodes {
        let Ok(content) = std::fs::read_to_string(&node.path) else { continue };
        if has_long_description(&content) {
            continue;
        }
        let updated = format!("{}\n\n# Long Description\nTBD\n", content.trim_end());
        if std::fs::write(&node.path, updated).is_ok() {
            stubbed.push(node.path.clone());
        }
    }
    stubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_comes_from_the_filename_suffix() {
        // The role adjacent to .iter.md wins; prefixes are free-form.
        assert_eq!(role_of("marker.iter.md"), Some(Role::Marker));
        assert_eq!(role_of("pdy_core_x.marker.iter.md"), Some(Role::Marker));
        assert_eq!(role_of("usecase.iter.md"), Some(Role::Usecase));
        assert_eq!(role_of("my_new.usecase.iter.md"), Some(Role::Usecase));
        assert_eq!(role_of("pdy_core_ingest_v2usecase.iter.md"), Some(Role::Usecase));
        assert_eq!(role_of("techreq.usecase.iter.md"), Some(Role::Usecase), "role next to .iter.md wins");
        assert_eq!(role_of("testgroup.iter.md"), Some(Role::Testgroup));
        assert_eq!(role_of("testgroups.iter.md"), None, "plural is NOT the singular role — plain doc");
        assert_eq!(role_of("devops.bizreq.iter.md"), Some(Role::Bizreq));
        assert_eq!(role_of("x.techreq.iter.md"), Some(Role::Techreq));
        assert_eq!(role_of("x.interface.iter.md"), Some(Role::Interface));
        assert_eq!(role_of("notes.iter.md"), None);
        assert_eq!(role_of("marker.md"), None, "must end .iter.md");
    }

    // Test-Loop gate resolution (features/test_loop_flag.md): every rule
    // proves it can fail.
    #[test]
    fn test_loop_nearest_wins_and_blocked_beats_include() {
        let mk = |key: &str, tl: &str| Node { key: key.into(), test_loop: tl.into(), ..Default::default() };
        let nodes = vec![
            mk("", ""),
            mk("core", TEST_LOOP_OMIT),
            mk("core/api", TEST_LOOP_INCLUDE),
            mk("core/db", ""),
            mk("vendor", TEST_LOOP_BLOCKED),
            mk("vendor/pay", TEST_LOOP_INCLUDE),
            mk("other", ""),
        ];
        let eff = |key: &str| effective_test_loop(nodes.iter().find(|n| n.key == key).unwrap(), &nodes);
        assert_eq!(eff("core/api"), TestLoopState::Included, "nearest include re-enters under an omitted ancestor");
        assert_eq!(eff("core/db"), TestLoopState::Omitted { value: "omit".into(), by: "core".into() }, "omit carries down");
        assert!(eff("core").is_omitted_test());
        assert_eq!(
            eff("vendor/pay"),
            TestLoopState::Omitted { value: "blocked".into(), by: "vendor".into() },
            "blocked beats a descendant include"
        );
        assert_eq!(eff("other"), TestLoopState::Included, "no flag anywhere = included");

        // Root-node ("") flags govern everything.
        let rooted = vec![mk("", TEST_LOOP_OMIT), mk("a", ""), mk("a/b", TEST_LOOP_INCLUDE)];
        let eff2 = |key: &str| effective_test_loop(rooted.iter().find(|n| n.key == key).unwrap(), &rooted);
        assert!(eff2("a").is_omitted_test(), "root omit reaches undecorated descendants");
        assert_eq!(eff2("a/b"), TestLoopState::Included, "…but a nearer include still wins");

        // Use-cases/interfaces: own flag only.
        assert!(own_test_loop("blocked", "f").is_omitted_test());
        assert!(own_test_loop("omit", "f").is_omitted_test());
        assert_eq!(own_test_loop("include", "f"), TestLoopState::Included);
        assert_eq!(own_test_loop("", "f"), TestLoopState::Included);
    }

    impl TestLoopState {
        fn is_omitted_test(&self) -> bool {
            matches!(self, TestLoopState::Omitted { .. })
        }
    }

    #[test]
    fn set_frontmatter_key_preserves_everything_else() {
        let dir = std::env::temp_dir().join(format!("iter-tlkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.marker.iter.md");
        std::fs::write(&f, "---\nname: \"X\"\nlevel: container\ntestgroup: test/testgroup.iter.md\n---\n\n# Body\nstays verbatim\n").unwrap();
        set_frontmatter_key(&f, "test_loop", Some("omit")).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("test_loop: omit"), "{}", text);
        assert!(text.contains("name: \"X\"") && text.contains("stays verbatim"), "{}", text);
        // Replace, not duplicate.
        set_frontmatter_key(&f, "test_loop", Some("include")).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert_eq!(text.matches("test_loop:").count(), 1, "{}", text);
        assert!(text.contains("test_loop: include"));
        // Clear removes the key entirely.
        set_frontmatter_key(&f, "test_loop", None).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains("test_loop"), "{}", text);
        assert!(text.contains("level: container") && text.contains("stays verbatim"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testloop_apply_guards_the_blocked_contract() {
        let dir = std::env::temp_dir().join(format!("iter-tlapply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["vendor/pay", "core"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let write = |rel: &str, tl: &str| {
            let path = dir.join(rel);
            let extra = if tl.is_empty() { String::new() } else { format!("test_loop: {}\n", tl) };
            std::fs::write(&path, format!("---\nname: \"n\"\nlevel: container\n{}---\nbody\n", extra)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let vendor = write("vendor/vendor.marker.iter.md", "blocked");
        let pay = write("vendor/pay/pay.marker.iter.md", "");
        let core = write("core/core.marker.iter.md", "omit");
        let mkn = |key: &str, path: &str, tl: &str| Node {
            key: key.into(),
            name: key.replace('/', "-"),
            path: path.into(),
            test_loop: tl.into(),
            ..Default::default()
        };
        let scan = Scan {
            nodes: vec![mkn("vendor", &vendor, "blocked"), mkn("vendor/pay", &pay, ""), mkn("core", &core, "omit")],
            ..Default::default()
        };
        // Blocked self: include/omit/clear all refuse; only a human file edit lifts it.
        for action in [TestLoopAction::Include, TestLoopAction::Omit, TestLoopAction::Clear] {
            let err = testloop_apply(&scan, "vendor", action).unwrap_err();
            assert!(err.contains("blocked"), "{:?}: {}", action, err);
        }
        // Include under a blocked ANCESTOR refuses too — it could never take effect.
        let err = testloop_apply(&scan, "vendor/pay", TestLoopAction::Include).unwrap_err();
        assert!(err.contains("blocked ancestor"), "{}", err);
        // Unknown and ambiguous refs refuse, never guess.
        assert!(testloop_apply(&scan, "nope", TestLoopAction::Omit).unwrap_err().contains("matches no"));
        assert!(testloop_apply(&scan, "marker.iter.md", TestLoopAction::Omit).unwrap_err().contains("ambiguous"));
        // A normal include lands in the file.
        let summary = testloop_apply(&scan, "core", TestLoopAction::Include).unwrap();
        assert!(summary.contains("include"), "{}", summary);
        assert!(std::fs::read_to_string(&core).unwrap().contains("test_loop: include"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stubs_only_nodes_missing_long_description() {
        let dir = std::env::temp_dir().join(format!("iter-stubld-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/a.marker.iter.md"), "---\nname: A\nlevel: component\n---\nbody only\n").unwrap();
        std::fs::write(dir.join("b/b.marker.iter.md"), "---\nname: B\nlevel: component\n---\n## Long Description\nAlready written.\n").unwrap();
        std::fs::write(dir.join("plain.iter.md"), "unrecognized suffix — not a node\n").unwrap();
        let s = scan(&dir, &[dir.clone()], "**/*.iter.md");
        let stubbed = stub_long_descriptions(&s);
        assert_eq!(stubbed.len(), 1, "only the node missing the section: {:?}", stubbed);
        let a = std::fs::read_to_string(dir.join("a/a.marker.iter.md")).unwrap();
        assert!(a.contains("# Long Description\nTBD"));
        // Idempotent: a second sweep stubs nothing.
        let s2 = scan(&dir, &[dir.clone()], "**/*.iter.md");
        assert!(stub_long_descriptions(&s2).is_empty());
        let plain = std::fs::read_to_string(dir.join("plain.iter.md")).unwrap();
        assert!(!plain.contains("Long Description"), "plain docs are never touched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tmpdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-markers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("core/intake")).unwrap();
        dir
    }

    #[test]
    fn stray_frontmatter_never_overrides_the_filename_role() {
        let dir = std::env::temp_dir().join(format!("iter-markers-tg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("core")).unwrap();
        std::fs::write(
            dir.join("core/core.marker.iter.md"),
            "---\nname: Core\nlevel: component\n---\nthe real node",
        )
        .unwrap();
        // An over-eager agent gave these files `level:` / `participants:` keys —
        // the FILENAME still decides: testgroup and usecase files never become
        // nodes; the only way to change role is to rename the file.
        std::fs::write(
            dir.join("core/testgroup.iter.md"),
            "---\nname: Core Tests\nlevel: component\n---\n<!-- iterapp:testgroups\n-->",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/flow.usecase.iter.md"),
            "---\nname: Flow\nlevel: container\nparticipants:\n  - 1 core\n---\nstory",
        )
        .unwrap();
        let scan = scan(&dir, &[dir.clone()], "**/*.iter.md");
        assert_eq!(scan.nodes.len(), 1, "only the *marker.iter.md file is a node");
        assert_eq!(scan.nodes[0].name, "Core");
        assert_eq!(scan.usecases.len(), 1, "the usecase file stays a use-case despite level:");
        assert!(
            scan.plain.iter().any(|p| p.ends_with("testgroup.iter.md")),
            "testgroup file is filed as plain context: {:?}",
            scan.plain
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roles_sorted_by_filename() {
        let dir = tmpdir();
        std::fs::write(
            dir.join("root.marker.iter.md"),
            "---\nname: My Project\nlevel: project\ndescription: \"top\"\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/intake/intake.marker.iter.md"),
            "---\nname: Intake\nlevel: container\nuses: [postgres, kafka]\n---\ncontext body",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/payout.usecase.iter.md"),
            "---\nname: Early payout\nparticipants:\n  - 1 core\n  - 2.1 core/intake\n---\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("core/evidence-api.interface.iter.md"),
            "---\ninterface: evidence-api\nkind: http\nendpoint: POST /v1/evidence\ndescription: \"store artifacts\"\n---\ncontract body",
        )
        .unwrap();
        std::fs::write(dir.join("core/bizreq.iter.md"), "requirement doc, plain to the structure view\n").unwrap();
        // A level: key on an unrecognized suffix does NOT make a node anymore.
        std::fs::write(dir.join("core/notes.iter.md"), "---\nlevel: component\n---\nnot a node\n").unwrap();

        let scan = scan(&dir, &[dir.clone()], "**/*.iter.md");
        assert_eq!(scan.nodes.len(), 2, "*marker.iter.md files are nodes");
        assert_eq!(scan.interfaces.len(), 1, "*interface.iter.md files aggregate globally");
        assert_eq!(scan.interfaces[0].id, "evidence-api");
        assert_eq!(scan.interfaces[0].kind, "http");
        assert_eq!(scan.interfaces[0].body, "contract body", "the contract itself rides along");
        assert_eq!(scan.nodes[1].body, "context body");
        assert_eq!(scan.nodes[0].key, "");
        assert_eq!(scan.nodes[0].name, "My Project");
        assert_eq!(scan.nodes[1].key, "core/intake");
        assert_eq!(scan.nodes[1].uses, vec!["postgres", "kafka"]);
        assert_eq!(scan.nodes[1].depth, 2);
        assert_eq!(scan.usecases.len(), 1, "*usecase.iter.md files are use-cases");
        assert_eq!(scan.usecases[0].steps.get("core/intake").unwrap(), "2.1");
        assert_eq!(scan.usecases[0].participants, vec!["1 core", "2.1 core/intake"], "raw ordered lines ride along");
        assert_eq!(scan.plain.len(), 2, "bizreq + unrecognized suffix are plain context");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_default_name_comes_from_the_filename() {
        let dir = std::env::temp_dir().join(format!("iter-markers-name-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("gw")).unwrap();
        std::fs::write(dir.join("gw/api_gateway.marker.iter.md"), "---\nlevel: container\n---\nbody").unwrap();
        std::fs::write(dir.join("marker.iter.md"), "---\nlevel: project\n---\nbody").unwrap();
        let s = scan(&dir, &[dir.clone()], "**/*.iter.md");
        let names: Vec<&str> = s.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"api_gateway"), "suffix stripped: {:?}", names);
        assert!(
            names.iter().any(|n| *n == dir.file_name().unwrap().to_str().unwrap()),
            "bare marker.iter.md falls back to its directory name: {:?}",
            names
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
