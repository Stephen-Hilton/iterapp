use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::placeholders::Vars;
use crate::project::Project;

/// structureV2 node discovery. Any file matching `{iterglob}` is
/// iterapp-meaningful, and its FILENAME declares what it IS via the explicit
/// dot rule: `*.nodetype.iter.md` — the nodetype segment must be preceded by a
/// dot unless the file has no prefix at all, and it is lowercase,
/// case-sensitively. Frontmatter supplies attributes, never identity.
///
/// Valid:   `my_thing.code.iter.md`, `code.iter.md`, `.code.iter.md`,
///          `my,super.duper.thing.code.iter.md`
/// Invalid: `my_thing_code.iter.md`, `my-code.iter.md`, `my_thing.Code.iter.md`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Main,
    Code,
    Bizreq,
    Techreq,
    Interface,
    Testgroup,
    Usecase,
}

pub fn role_of(filename: &str) -> Option<Role> {
    let stem = filename.strip_suffix(".iter.md")?;
    // The nodetype is the segment after the LAST dot (the whole stem when the
    // file has no prefix). Case-sensitive: `Code` is not a nodetype.
    let tag = stem.rsplit('.').next().unwrap_or(stem);
    match tag {
        "main" => Some(Role::Main),
        "code" => Some(Role::Code),
        "bizreq" => Some(Role::Bizreq),
        "techreq" => Some(Role::Techreq),
        "interface" => Some(Role::Interface),
        "testgroup" => Some(Role::Testgroup),
        "usecase" => Some(Role::Usecase),
        _ => None,
    }
}

/// `{thisfilestem}`: the filename minus `.iter.md` AND minus the nodetype
/// segment — `mylib.code.iter.md` → `mylib`, bare `code.iter.md` → `""`.
pub fn stem_of(filename: &str) -> String {
    let Some(stem) = filename.strip_suffix(".iter.md") else {
        return filename.trim_end_matches(".md").to_string();
    };
    if role_of(filename).is_none() {
        return stem.to_string();
    }
    match stem.rfind('.') {
        Some(i) => stem[..i].to_string(),
        None => String::new(),
    }
}

pub fn role_name(role: Option<Role>) -> &'static str {
    match role {
        Some(Role::Main) => "main",
        Some(Role::Code) => "code",
        Some(Role::Bizreq) => "bizreq",
        Some(Role::Techreq) => "techreq",
        Some(Role::Interface) => "interface",
        Some(Role::Testgroup) => "testgroup",
        Some(Role::Usecase) => "usecase",
        None => "plain (no-role)",
    }
}

/* ------------------------------------------------------------ frontmatter */

/// Parsed frontmatter: flat scalars, top-level lists, and the `children:`
/// mapping of typed link-lists. A single string where a list is expected is
/// accepted and coerced to a 1-item list (structureV2 forgiveness rule).
#[derive(Debug, Clone, Default)]
pub struct Front {
    pub scalars: HashMap<String, String>,
    pub lists: HashMap<String, Vec<String>>,
    pub children: HashMap<String, Vec<String>>,
    /// Was a `children:` key present at all (even empty)?
    pub children_present: bool,
    pub body: String,
    /// Did the file have a parseable `---` fence at all?
    pub has_frontmatter: bool,
}

impl Front {
    pub fn scalar(&self, key: &str) -> String {
        self.scalars.get(key).cloned().unwrap_or_default()
    }
    /// A top-level list — or a scalar coerced to one item.
    pub fn list(&self, key: &str) -> Vec<String> {
        if let Some(l) = self.lists.get(key) {
            return l.clone();
        }
        match self.scalars.get(key) {
            Some(s) if !s.trim().is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    }
    /// A `children:` sub-key: None = absent (defaults apply), Some = declared
    /// (possibly empty).
    pub fn child(&self, key: &str) -> Option<Vec<String>> {
        self.children.get(key).cloned()
    }
    /// `teststate:` with graceful V1 fallback: an unmigrated `test_loop:` key
    /// still reads, with the old `blocked` value mapped to `block`.
    pub fn teststate(&self) -> String {
        let v = self.scalar("teststate");
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
        match self.scalar("test_loop").trim() {
            "blocked" => TS_BLOCK.to_string(),
            other => other.to_string(),
        }
    }
}

/// Body cap per node in scan responses: bodies ride into the UI/API in full,
/// but a runaway file must not bloat every response.
const BODY_CAP: usize = 65536;

fn clean_value(raw: &str) -> String {
    let raw = raw.trim();
    if raw.len() >= 2
        && ((raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('`') && raw.ends_with('`')))
    {
        return raw[1..raw.len() - 1].to_string();
    }
    // YAML comment rule: an unquoted value ends at the first ` #`.
    match raw.find(" #") {
        Some(i) => raw[..i].trim_end().to_string(),
        None => raw.to_string(),
    }
}

fn parse_inline_list(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| clean_value(s.trim()))
        .filter(|s| !s.is_empty() && s != "...")
        .collect()
}

#[derive(PartialEq)]
enum FrontMode {
    Top,
    List(String),
    Children,
    ChildList(String),
}

/// Parse `---`-fenced frontmatter (structureV2 "Common .iter.md File
/// Structure"): flat `key: value` scalars, `key: [a, b]` / block lists, and
/// the `children:` mapping whose indented sub-keys are the typed link-lists.
pub fn parse_front(content: &str) -> Front {
    let mut front = Front::default();
    let trimmed = content.trim_start_matches(['\u{feff}', ' ', '\t', '\n', '\r']);
    let Some(rest) = trimmed.strip_prefix("---") else {
        front.body = content.trim().to_string();
        if front.body.len() > BODY_CAP {
            front.body.truncate(BODY_CAP);
        }
        return front;
    };
    let Some(end) = rest.find("\n---") else { return front };
    front.has_frontmatter = true;
    let mut body = rest[end + 4..].trim_start_matches('-').trim().to_string();
    if body.len() > BODY_CAP {
        body.truncate(BODY_CAP);
        body.push_str("\n… (truncated — read the full file on disk)");
    }
    front.body = body;

    let mut mode = FrontMode::Top;
    for line in rest[..end].lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            mode = FrontMode::Top; // top level closes any open list / children block
            if t == "children:" {
                front.children_present = true;
                mode = FrontMode::Children;
                continue;
            }
            if let Some((key, val)) = t.split_once(':') {
                let key = key.trim().to_string();
                let val = val.trim();
                if val.is_empty() {
                    front.lists.insert(key.clone(), Vec::new());
                    mode = FrontMode::List(key);
                } else if val.starts_with('[') {
                    front.lists.insert(key, parse_inline_list(val));
                } else {
                    front.scalars.insert(key, clean_value(val));
                }
            }
            continue;
        }
        // Indented lines belong to whatever block is open.
        match &mode {
            FrontMode::Top => {}
            FrontMode::List(key) => {
                if let Some(item) = t.strip_prefix("- ") {
                    front.lists.get_mut(key).expect("open list").push(clean_value(item));
                }
            }
            FrontMode::Children | FrontMode::ChildList(_) => {
                let (line_t, bulleted) = match t.strip_prefix("- ") {
                    Some(rest) => (rest.trim(), true),
                    None => (t, false),
                };
                // A sub-key line (`subkey: [...]` / `subkey: x` / `subkey:`),
                // with the `- subkey:` bullet style tolerated too.
                let subkey = line_t
                    .split_once(':')
                    .filter(|(k, _)| !k.trim().is_empty() && !k.trim().contains(' '));
                match subkey {
                    Some((key, val)) => {
                        let key = key.trim().to_string();
                        let val = val.trim();
                        if val.is_empty() {
                            front.children.insert(key.clone(), Vec::new());
                            mode = FrontMode::ChildList(key);
                        } else if val.starts_with('[') {
                            front.children.insert(key, parse_inline_list(val));
                            mode = FrontMode::Children;
                        } else {
                            front.children.insert(key, vec![clean_value(val)]);
                            mode = FrontMode::Children;
                        }
                    }
                    None => {
                        if bulleted {
                            if let FrontMode::ChildList(key) = &mode {
                                front
                                    .children
                                    .get_mut(key)
                                    .expect("open child list")
                                    .push(clean_value(line_t));
                            }
                        }
                    }
                }
            }
        }
    }
    front
}

/* ------------------------------------------------------------ scan types */

#[derive(Debug, Clone, Serialize, Default)]
pub struct Node {
    /// DAG path: parent key + "/" + this node's slug; contexts sit at depth 0.
    pub key: String,
    /// Primary parent's key ("" = root-owned context).
    pub parent: String,
    /// Every parent key (primary first) — a DAG allows more than one.
    pub parents: Vec<String>,
    pub name: String,
    pub level: String,
    pub description: String,
    pub owner: String,
    /// Own `teststate:` flag (omit|include|block|inherit; "" = inherit).
    pub teststate: String,
    /// Interface IDs consumed / produced by this node's code.
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Resolved absolute source directories (workitem codepaths).
    pub codedirs: Vec<String>,
    /// Resolved child code node file paths.
    pub codenodes: Vec<String>,
    pub bizreqs: Vec<String>,
    pub techreqs: Vec<String>,
    /// Resolved testgroup.iter.md file paths.
    pub testgroups: Vec<String>,
    /// Did the file DECLARE `children.testgroups` (vs the default glob)?
    pub testgroups_declared: bool,
    /// Declared testgroup entries that matched nothing (expanded form).
    pub missing_testgroups: Vec<String>,
    pub dir: String,  // the node file's own directory ({thisfiledir})
    pub path: String, // absolute node file path
    pub depth: usize,
    pub body: String,
    /// Effective teststate after DAG resolution — "included", or
    /// "omit via <key>" / "block via <key>" — so UIs need not re-derive it.
    pub teststate_effective: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Interface {
    pub id: String,
    pub kind: String,
    pub endpoint: String,
    pub description: String,
    pub owner: String,
    pub teststate: String,
    pub testgroups: Vec<String>,
    pub testgroups_declared: bool,
    pub missing_testgroups: Vec<String>,
    pub bizreqs: Vec<String>,
    pub techreqs: Vec<String>,
    pub file: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UseCase {
    pub name: String,
    pub description: String,
    pub file: String,
    pub teststate: String,
    /// Resolved code node file paths (required key; may be empty — an empty
    /// list is valid and marks agent work to come).
    pub codenodes: Vec<String>,
    /// The same, resolved to DAG node keys where possible.
    pub codenode_keys: Vec<String>,
    pub codenodes_declared: bool,
    pub testgroups: Vec<String>,
    pub testgroups_declared: bool,
    pub missing_testgroups: Vec<String>,
    pub body: String,
}

/// A file matching the glob + naming rules that is NOT linked into the DAG.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Orphan {
    pub path: String,
    pub role: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Scan {
    pub nodes: Vec<Node>,
    pub interfaces: Vec<Interface>,
    pub usecases: Vec<UseCase>,
    /// .iter.md files whose name matches no nodetype: plain context docs.
    pub plain: Vec<String>,
    pub orphans: Vec<Orphan>,
    pub notes: Vec<String>,
    pub roots: Vec<String>,
}

/* ------------------------------------------------------------ scan */

const DEPTH_SOFT_CAP: usize = 10;

struct RawCode {
    path: PathBuf,
    dir: PathBuf,
    name: String,
    level: String,
    description: String,
    owner: String,
    teststate: String,
    codedirs: Vec<PathBuf>,
    codenodes: Vec<PathBuf>,
    input_files: Vec<PathBuf>,
    output_files: Vec<PathBuf>,
    bizreqs: Vec<PathBuf>,
    techreqs: Vec<PathBuf>,
    testgroups: Vec<PathBuf>,
    testgroups_declared: bool,
    missing_testgroups: Vec<String>,
    body: String,
}

/// Resolve one children sub-key: declared patterns (or the default), each
/// expanded per-file with the placeholder vars, rglob'd to existing files.
/// Declared entries that match nothing are recorded (first expansion) so
/// coverage gaps surface instead of silently vanishing.
fn resolve_child_files(
    front: &Front,
    key: &str,
    default: &[&str],
    vars: &Vars,
    base: &Path,
) -> (Vec<PathBuf>, bool, Vec<String>) {
    let (patterns, declared) = match front.child(key) {
        Some(p) => (p, true),
        None => (default.iter().map(|s| s.to_string()).collect(), false),
    };
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for pattern in &patterns {
        if pattern.trim().is_empty() || pattern.trim() == "none" {
            continue;
        }
        let hits = vars.expand_files(pattern, base);
        // Only an EXPLICIT (non-glob) declared entry that matches nothing is a
        // gap: a declared glob matching nothing is just "no files yet" — the
        // normal state of a node whose defaults were written out explicitly.
        let is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        if hits.is_empty() && declared && !is_glob {
            missing.push(vars.expand(pattern).pop().unwrap_or_else(|| pattern.clone()));
        }
        files.extend(hits);
    }
    files.sort();
    files.dedup();
    (files, declared, missing)
}

fn filesafe(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ','))
        .collect();
    s
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Load a project's whole DAG: one call that reads the head files and scans.
pub fn scan_project(project_root: &Path) -> (Project, Scan) {
    let project = Project::load(project_root);
    let scan = scan(&project);
    (project, scan)
}

/// The structureV2 ingest: gather every `{iterglob}` file under the
/// `globalscandirs`, classify by filename nodetype, resolve each node's
/// `children` links (explicit links are the ONLY joining mechanism — directory
/// nesting alone links nothing), then assemble the DAG:
///
/// - `context`-level code nodes attach to the root (the project server).
/// - container/component code nodes attach where a parent's `codenodes` lists
///   them; unreferenced ones land in the Orphanage.
/// - interfaces and use-cases are global objects: always root children.
/// - a cycle demotes the offending edge (noted) — the stranded subtree
///   orphans rather than failing ingest.
/// - bizreq/techreq/testgroup files claimed by no linked node orphan too.
pub fn scan(project: &Project) -> Scan {
    let mut scan = Scan::default();
    let base_vars = project.vars();
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &project.scandirs {
        scan.roots.push(dir.to_string_lossy().into_owned());
        let pattern = format!("{}/{}", dir.to_string_lossy(), project.server.iterglob);
        files.extend(base_vars.expand_files(&pattern, dir));
    }
    if project.mainfile.is_file() {
        files.push(project.mainfile.clone());
    }
    let mut files: Vec<PathBuf> = files.iter().map(|p| canon(p)).collect();
    files.sort();
    files.dedup();

    let mainfile = canon(&project.mainfile);
    let mut raw_codes: Vec<RawCode> = Vec::new();
    let mut iface_files: Vec<(PathBuf, Front)> = Vec::new();
    let mut usecase_files: Vec<(PathBuf, Front)> = Vec::new();
    let mut other_roled: Vec<(PathBuf, Role)> = Vec::new();

    for path in &files {
        let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let role = role_of(&fname);
        if *path == mainfile {
            continue; // the root itself, not a node
        }
        let Ok(content) = std::fs::read_to_string(path) else { continue };
        match role {
            None => scan.plain.push(path.to_string_lossy().into_owned()),
            Some(Role::Main) => {
                scan.orphans.push(Orphan {
                    path: path.to_string_lossy().into_owned(),
                    role: "main".into(),
                    name: fname.clone(),
                    reason: format!(
                        "a second main file — the project head is {} (config.iter.json mainfile)",
                        mainfile.display()
                    ),
                });
            }
            Some(Role::Code) => {
                let front = parse_front(&content);
                let dir = path.parent().map(PathBuf::from).unwrap_or_default();
                let fvars = base_vars.with_file(path);
                let codedir_patterns =
                    front.child("codedirs").unwrap_or_else(|| vec!["{thisfiledir}/".into()]);
                let mut codedirs: Vec<PathBuf> = Vec::new();
                for p in &codedir_patterns {
                    codedirs.extend(fvars.expand_dirs(p, &dir));
                }
                codedirs.sort();
                codedirs.dedup();
                // {codedirs} becomes a list placeholder for the other sub-keys.
                let mut cvars = fvars.clone();
                cvars.set_list(
                    "codedirs",
                    &codedirs.iter().map(|d| format!("{}/", d.to_string_lossy())).collect::<Vec<_>>(),
                );
                let (codenodes, _, _) = resolve_child_files(&front, "codenodes", &[], &cvars, &dir);
                let (input_files, _, _) = resolve_child_files(&front, "inputs", &[], &cvars, &dir);
                let (output_files, _, _) = resolve_child_files(&front, "outputs", &[], &cvars, &dir);
                let (bizreqs, _, _) = resolve_child_files(
                    &front, "bizreqs", &["{thisfiledir}/*.bizreq.iter.md"], &cvars, &dir,
                );
                let (techreqs, _, _) = resolve_child_files(
                    &front, "techreqs", &["{thisfiledir}/*.techreq.iter.md"], &cvars, &dir,
                );
                let (testgroups, testgroups_declared, missing_testgroups) = resolve_child_files(
                    &front, "testgroups", &["{thisfiledir}/test/*.testgroup.iter.md"], &cvars, &dir,
                );
                let name = {
                    let n = front.scalar("name");
                    if !n.trim().is_empty() {
                        n
                    } else {
                        let stem = stem_of(&fname);
                        if stem.is_empty() {
                            dir.file_name().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
                        } else {
                            stem
                        }
                    }
                };
                raw_codes.push(RawCode {
                    path: path.clone(),
                    dir,
                    name,
                    level: front.scalar("level"),
                    description: front.scalar("description"),
                    owner: front.scalar("owner"),
                    teststate: front.teststate(),
                    codedirs,
                    codenodes: codenodes.iter().map(|p| canon(p)).collect(),
                    input_files,
                    output_files,
                    bizreqs,
                    techreqs,
                    testgroups,
                    testgroups_declared,
                    missing_testgroups,
                    body: front.body,
                });
            }
            Some(Role::Interface) => iface_files.push((path.clone(), parse_front(&content))),
            Some(Role::Usecase) => usecase_files.push((path.clone(), parse_front(&content))),
            Some(r @ (Role::Bizreq | Role::Techreq | Role::Testgroup)) => {
                other_roled.push((path.clone(), r));
            }
        }
    }

    /* ---- interfaces & use-cases: global objects, always root children ---- */

    let mut iface_id_by_path: HashMap<PathBuf, String> = HashMap::new();
    for (path, front) in &iface_files {
        let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        let fvars = base_vars.with_file(path);
        let id = {
            let n = front.scalar("name");
            let n = if n.trim().is_empty() { front.scalar("interface") } else { n }; // V1 fallback
            if n.trim().is_empty() { stem_of(&fname) } else { n.trim().to_string() }
        };
        iface_id_by_path.insert(canon(path), id.clone());
        let (testgroups, testgroups_declared, missing_testgroups) = resolve_child_files(
            front, "testgroups", &["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"], &fvars, &dir,
        );
        let (bizreqs, _, _) = resolve_child_files(
            front, "bizreqs", &["{thisfiledir}/{thisfilestem}/*.bizreq.iter.md"], &fvars, &dir,
        );
        let (techreqs, _, _) = resolve_child_files(
            front, "techreqs", &["{thisfiledir}/{thisfilestem}/*.techreq.iter.md"], &fvars, &dir,
        );
        scan.interfaces.push(Interface {
            id,
            kind: front.scalar("kind"),
            endpoint: front.scalar("endpoint"),
            description: front.scalar("description"),
            owner: front.scalar("owner"),
            teststate: front.teststate(),
            testgroups: testgroups.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            testgroups_declared,
            missing_testgroups,
            bizreqs: bizreqs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            techreqs: techreqs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            file: path.to_string_lossy().into_owned(),
            body: front.body.clone(),
        });
    }
    scan.interfaces.sort_by(|a, b| a.id.cmp(&b.id));

    for (path, front) in &usecase_files {
        let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        let fvars = base_vars.with_file(path);
        let (codenodes, codenodes_declared, _) = resolve_child_files(front, "codenodes", &[], &fvars, &dir);
        let (testgroups, testgroups_declared, missing_testgroups) = resolve_child_files(
            front, "testgroups", &["{thisfiledir}/{thisfilestem}/*.testgroup.iter.md"], &fvars, &dir,
        );
        let name = {
            let n = front.scalar("name");
            if n.trim().is_empty() { stem_of(&fname) } else { n }
        };
        scan.usecases.push(UseCase {
            name,
            description: front.scalar("description"),
            file: path.to_string_lossy().into_owned(),
            teststate: front.teststate(),
            codenodes: codenodes.iter().map(|p| canon(p).to_string_lossy().into_owned()).collect(),
            codenode_keys: Vec::new(), // filled after the DAG assembles
            codenodes_declared,
            testgroups: testgroups.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            testgroups_declared,
            missing_testgroups,
            body: front.body.clone(),
        });
    }
    scan.usecases.sort_by(|a, b| a.name.cmp(&b.name));

    /* ---- code DAG assembly ---- */

    let idx_by_path: HashMap<PathBuf, usize> =
        raw_codes.iter().enumerate().map(|(i, r)| (canon(&r.path), i)).collect();
    let mut key_of: HashMap<usize, String> = HashMap::new(); // raw idx → key
    let mut used_keys: HashSet<String> = HashSet::new();
    let mut out_nodes: Vec<Node> = Vec::new();
    let mut out_idx: HashMap<usize, usize> = HashMap::new(); // raw idx → out idx

    // Root children (the Ownership Tree): main.iter.md may list code nodes of
    // ANY level in its own `children.codenodes` — the "moved up to belong to
    // root" mechanism — and `context`-level nodes attach to root by default
    // UNLESS some other code node references them (a context "moved down").
    let main_codenodes: Vec<PathBuf> = if mainfile.is_file() {
        std::fs::read_to_string(&mainfile)
            .ok()
            .map(|t| {
                let front = parse_front(&t);
                let fvars = base_vars.with_file(&mainfile);
                let dir = mainfile.parent().map(PathBuf::from).unwrap_or_default();
                let (files, _, _) = resolve_child_files(&front, "codenodes", &[], &fvars, &dir);
                files.iter().map(|p| canon(p)).collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let referenced: HashSet<PathBuf> =
        raw_codes.iter().flat_map(|r| r.codenodes.iter().map(|p| canon(p))).collect();
    let mut context_idx: Vec<usize> = main_codenodes
        .iter()
        .filter_map(|p| idx_by_path.get(p).copied())
        .collect();
    context_idx.extend(
        raw_codes
            .iter()
            .enumerate()
            .filter(|(_, r)| r.level.trim() == "context" && !referenced.contains(&canon(&r.path)))
            .map(|(i, _)| i),
    );
    context_idx.sort_by(|a, b| raw_codes[*a].path.cmp(&raw_codes[*b].path));
    context_idx.dedup();

    // Iterative DFS with an explicit on-path set for cycle demotion.
    fn attach(
        raw_idx: usize,
        parent_key: &str,
        depth: usize,
        raws: &[RawCode],
        idx_by_path: &HashMap<PathBuf, usize>,
        key_of: &mut HashMap<usize, String>,
        used_keys: &mut HashSet<String>,
        out_nodes: &mut Vec<Node>,
        out_idx: &mut HashMap<usize, usize>,
        on_path: &mut HashSet<usize>,
        notes: &mut Vec<String>,
    ) {
        if let Some(&oi) = out_idx.get(&raw_idx) {
            // Already in the DAG: record the extra parent, don't re-descend.
            if !parent_key.is_empty() && !out_nodes[oi].parents.contains(&parent_key.to_string()) {
                out_nodes[oi].parents.push(parent_key.to_string());
            }
            return;
        }
        let raw = &raws[raw_idx];
        let fname = raw.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let slug = {
            let s = stem_of(&fname);
            let s = if s.is_empty() {
                raw.dir.file_name().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
            } else {
                s
            };
            let s = filesafe(&s);
            if s.is_empty() { "node".to_string() } else { s }
        };
        let base_key = if parent_key.is_empty() { slug.clone() } else { format!("{}/{}", parent_key, slug) };
        let mut key = base_key.clone();
        let mut n = 2;
        while !used_keys.insert(key.clone()) {
            key = format!("{}-{}", base_key, n);
            n += 1;
        }
        if depth > DEPTH_SOFT_CAP {
            notes.push(format!(
                "depth {} exceeds the soft cap of {} layers at {} — flatten the structure",
                depth, DEPTH_SOFT_CAP, key
            ));
        }
        key_of.insert(raw_idx, key.clone());
        let node = Node {
            key: key.clone(),
            parent: parent_key.to_string(),
            parents: if parent_key.is_empty() { Vec::new() } else { vec![parent_key.to_string()] },
            name: raw.name.clone(),
            level: raw.level.clone(),
            description: raw.description.clone(),
            owner: raw.owner.clone(),
            teststate: raw.teststate.clone(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            codedirs: raw.codedirs.iter().map(|d| d.to_string_lossy().into_owned()).collect(),
            codenodes: raw.codenodes.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            bizreqs: raw.bizreqs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            techreqs: raw.techreqs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            testgroups: raw.testgroups.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            testgroups_declared: raw.testgroups_declared,
            missing_testgroups: raw.missing_testgroups.clone(),
            dir: raw.dir.to_string_lossy().into_owned(),
            path: raw.path.to_string_lossy().into_owned(),
            depth,
            body: raw.body.clone(),
            teststate_effective: String::new(), // filled after the full DAG assembles
        };
        out_idx.insert(raw_idx, out_nodes.len());
        out_nodes.push(node);
        on_path.insert(raw_idx);
        for child_path in &raw.codenodes {
            let Some(&child_idx) = idx_by_path.get(child_path) else {
                notes.push(format!(
                    "{}: codenodes entry {} is not a code node file",
                    raws[raw_idx].path.display(),
                    child_path.display()
                ));
                continue;
            };
            if on_path.contains(&child_idx) {
                notes.push(format!(
                    "cycle demoted: edge {} → {} would recurse; the link is ignored (fix the codenodes)",
                    raws[raw_idx].path.display(),
                    child_path.display()
                ));
                continue;
            }
            attach(
                child_idx, &key, depth + 1, raws, idx_by_path, key_of, used_keys, out_nodes, out_idx,
                on_path, notes,
            );
        }
        on_path.remove(&raw_idx);
    }

    let mut on_path: HashSet<usize> = HashSet::new();
    for ci in context_idx {
        attach(
            ci, "", 0, &raw_codes, &idx_by_path, &mut key_of, &mut used_keys, &mut out_nodes,
            &mut out_idx, &mut on_path, &mut scan.notes,
        );
    }
    // Fallback pass: a context referenced ONLY from unreachable nodes (e.g. a
    // cycle among themselves) would otherwise vanish entirely — attach any
    // still-unplaced context at root so only the bad EDGE demotes, never the
    // whole subtree.
    let mut stranded: Vec<usize> = raw_codes
        .iter()
        .enumerate()
        .filter(|(i, r)| r.level.trim() == "context" && !out_idx.contains_key(i))
        .map(|(i, _)| i)
        .collect();
    stranded.sort_by(|a, b| raw_codes[*a].path.cmp(&raw_codes[*b].path));
    for ci in stranded {
        attach(
            ci, "", 0, &raw_codes, &idx_by_path, &mut key_of, &mut used_keys, &mut out_nodes,
            &mut out_idx, &mut on_path, &mut scan.notes,
        );
    }

    // inputs/outputs: interface files → ids (unknown files keep their stem).
    for (raw_idx, &oi) in &out_idx {
        let raw = &raw_codes[*raw_idx];
        let to_ids = |files: &[PathBuf]| -> Vec<String> {
            files
                .iter()
                .map(|f| {
                    iface_id_by_path.get(&canon(f)).cloned().unwrap_or_else(|| {
                        stem_of(&f.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default())
                    })
                })
                .collect()
        };
        out_nodes[oi].inputs = to_ids(&raw.input_files);
        out_nodes[oi].outputs = to_ids(&raw.output_files);
    }

    // Use-case codenode links → DAG keys.
    let key_by_path: HashMap<String, String> = out_idx
        .iter()
        .map(|(ri, oi)| (canon(&raw_codes[*ri].path).to_string_lossy().into_owned(), out_nodes[*oi].key.clone()))
        .collect();
    for uc in &mut scan.usecases {
        uc.codenode_keys = uc
            .codenodes
            .iter()
            .filter_map(|p| key_by_path.get(p).cloned())
            .collect();
    }

    /* ---- orphanage ---- */

    let mut claimed: HashSet<PathBuf> = HashSet::new();
    claimed.insert(mainfile.clone());
    // Files the project head pins into every agent context are linked BY the
    // head — global reqs must not read as orphans.
    for f in project.context_files() {
        claimed.insert(canon(&f));
    }
    for (ri, _) in &out_idx {
        let raw = &raw_codes[*ri];
        claimed.insert(canon(&raw.path));
        for p in raw
            .codenodes
            .iter()
            .chain(raw.input_files.iter())
            .chain(raw.output_files.iter())
            .chain(raw.bizreqs.iter())
            .chain(raw.techreqs.iter())
            .chain(raw.testgroups.iter())
        {
            claimed.insert(canon(p));
        }
    }
    for i in &scan.interfaces {
        claimed.insert(canon(Path::new(&i.file)));
        for p in i.testgroups.iter().chain(i.bizreqs.iter()).chain(i.techreqs.iter()) {
            claimed.insert(canon(Path::new(p)));
        }
    }
    for u in &scan.usecases {
        claimed.insert(canon(Path::new(&u.file)));
        for p in u.testgroups.iter() {
            claimed.insert(canon(Path::new(p)));
        }
    }
    for (i, raw) in raw_codes.iter().enumerate() {
        if out_idx.contains_key(&i) {
            continue;
        }
        let reason = if raw.level.trim() == "context" {
            "context node lost to a demoted cycle edge".to_string()
        } else {
            format!(
                "no linked code node lists this file in its codenodes (level: {})",
                if raw.level.trim().is_empty() { "unset" } else { raw.level.trim() }
            )
        };
        scan.orphans.push(Orphan {
            path: raw.path.to_string_lossy().into_owned(),
            role: "code".into(),
            name: raw.name.clone(),
            reason,
        });
    }
    for (path, role) in &other_roled {
        if !claimed.contains(&canon(path)) {
            let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            scan.orphans.push(Orphan {
                path: path.to_string_lossy().into_owned(),
                role: role_name(Some(*role)).into(),
                name: stem_of(&fname),
                reason: "no linked node's children patterns match this file".into(),
            });
        }
    }
    scan.orphans.sort_by(|a, b| a.path.cmp(&b.path));

    out_nodes.sort_by(|a, b| a.key.cmp(&b.key));
    for i in 0..out_nodes.len() {
        out_nodes[i].teststate_effective = match effective_teststate(&out_nodes[i], &out_nodes) {
            TestState::Included => "included".into(),
            TestState::Omitted { value, by } => format!("{} via {}", value, by),
        };
    }
    scan.nodes = out_nodes;
    scan
}

/* --------------------------- teststate gate (was test_loop) ---------------
`teststate:` parks nodes out of the deterministic test sweep without removing
anything: "omit" (workflow parking; agents may flip it back), "include"
(re-enter under an omitted ancestor), "block" (hard park — vendor/outside
setup missing; beats every descendant include, and only a human editing the
file lifts it), "inherit"/"" (default; do whatever your parent did). */

pub const TS_OMIT: &str = "omit";
pub const TS_INCLUDE: &str = "include";
pub const TS_BLOCK: &str = "block";
pub const TS_INHERIT: &str = "inherit";

#[derive(Debug, Clone, PartialEq)]
pub enum TestState {
    Included,
    /// value = "omit" | "block"; by = key (or file) of the deciding node.
    Omitted { value: String, by: String },
}

fn flag_of(raw: &str) -> &str {
    match raw.trim() {
        TS_INHERIT => "",
        other => other,
    }
}

/// Effective teststate of a code node in the DAG. `block` anywhere on ANY
/// ancestor chain (self included) wins. Otherwise a node is INCLUDED if any
/// chain to the root resolves included (nearest explicit omit/include on the
/// chain decides; no flag anywhere = included) — a DAG node shared by an
/// omitting parent and an including parent is tested via the including
/// chain's runs and skipped via the omitting one's, which the sweep resolves
/// to "runs".
pub fn effective_teststate(node: &Node, nodes: &[Node]) -> TestState {
    let by_key: HashMap<&str, &Node> = nodes.iter().map(|n| (n.key.as_str(), n)).collect();

    // block: self or any transitive ancestor.
    let mut stack = vec![node.key.as_str()];
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(k) = stack.pop() {
        if !seen.insert(k) {
            continue;
        }
        let Some(n) = by_key.get(k) else { continue };
        if flag_of(&n.teststate) == TS_BLOCK {
            return TestState::Omitted { value: TS_BLOCK.into(), by: n.key.clone() };
        }
        stack.extend(n.parents.iter().map(|p| p.as_str()));
    }

    fn chain_included<'a>(
        key: &'a str,
        by_key: &HashMap<&'a str, &'a Node>,
        memo: &mut HashMap<&'a str, bool>,
    ) -> bool {
        if let Some(&v) = memo.get(key) {
            return v;
        }
        memo.insert(key, true); // provisional (cycles were demoted at ingest)
        let Some(n) = by_key.get(key) else { return true };
        let v = match flag_of(&n.teststate) {
            TS_INCLUDE => true,
            TS_OMIT => false,
            _ => {
                if n.parents.is_empty() {
                    true
                } else {
                    n.parents.iter().any(|p| {
                        let p: &str = by_key.get(p.as_str()).map(|n| n.key.as_str()).unwrap_or("");
                        p.is_empty() || chain_included(p, by_key, memo)
                    })
                }
            }
        };
        memo.insert(key, v);
        v
    }
    let mut memo = HashMap::new();
    if chain_included(node.key.as_str(), &by_key, &mut memo) {
        return TestState::Included;
    }
    // Name the nearest omit for the message.
    let mut stack = vec![node.key.as_str()];
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(k) = stack.pop() {
        if !seen.insert(k) {
            continue;
        }
        let Some(n) = by_key.get(k) else { continue };
        if flag_of(&n.teststate) == TS_OMIT {
            return TestState::Omitted { value: TS_OMIT.into(), by: n.key.clone() };
        }
        stack.extend(n.parents.iter().map(|p| p.as_str()));
    }
    TestState::Omitted { value: TS_OMIT.into(), by: node.key.clone() }
}

/// Teststate of a use-case or interface: own flag only — global objects the
/// hierarchy doesn't own get no ancestry.
pub fn own_teststate(teststate: &str, file: &str) -> TestState {
    match flag_of(teststate) {
        v @ (TS_OMIT | TS_BLOCK) => TestState::Omitted { value: v.into(), by: file.to_string() },
        _ => TestState::Included,
    }
}

/// Deterministically set (Some) or remove (None) one scalar frontmatter key,
/// preserving every other line verbatim. The file must already have a
/// frontmatter fence.
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
        // Only TOP-LEVEL lines match: children sub-keys are indented.
        let is_top = line.len() == line.trim_start().len();
        if is_top && line.starts_with(&prefix) {
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

/// Deterministically set one `children:` sub-key to an inline list, preserving
/// every other line verbatim — the engine-owned write path for link edits
/// (e.g. `iter usecase --add` editing codenodes). Creates the `children:`
/// block when missing. The file must already have a frontmatter fence.
pub fn set_children_key(path: &Path, key: &str, values: &[String]) -> Result<(), String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return Err(format!("{} has no frontmatter fence", path.display()));
    };
    let Some(end) = rest.find("\n---") else {
        return Err(format!("{} has no closing frontmatter fence", path.display()));
    };
    let rendered = {
        let parts: Vec<String> = values
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{}\"", s.replace('"', " ")))
            .collect();
        format!("  {}: [{}]", key, parts.join(", "))
    };
    let mut kept: Vec<String> = Vec::new();
    let mut in_children = false;
    let mut in_this_key = false;
    let mut wrote = false;
    for line in rest[..end].lines() {
        let indent0 = line.len() == line.trim_start().len();
        let t = line.trim();
        if indent0 {
            in_children = t == "children:";
            in_this_key = false;
            kept.push(line.to_string());
            if in_children && !wrote {
                kept.push(rendered.clone());
                wrote = true;
            }
            continue;
        }
        if in_children {
            let sub = t.trim_start_matches("- ").trim();
            if let Some((k, _)) = sub.split_once(':') {
                in_this_key = k.trim() == key;
                if in_this_key {
                    continue; // replaced by the rendered line
                }
            } else if in_this_key && t.starts_with("- ") {
                continue; // old block-list items of the replaced key
            } else {
                in_this_key = false;
            }
        }
        kept.push(line.to_string());
    }
    if !wrote {
        kept.push("children:".to_string());
        kept.push(rendered);
    }
    let new_content = format!("---{}\n---{}", kept.join("\n"), &rest[end + 4..]);
    std::fs::write(path, new_content).map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

/// A `teststate` edit action, shared by `iter teststate` and POST /api/teststate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestStateAction {
    Omit,
    Include,
    Block,
    Clear,
}

struct TestStateTarget {
    label: String,
    path: String,
    current: String,
    node_key: Option<String>,
}

fn resolve_teststate_target(scan: &Scan, target: &str) -> Result<TestStateTarget, String> {
    let t = target.trim().trim_end_matches('/');
    let mut matches: Vec<TestStateTarget> = Vec::new();
    for n in &scan.nodes {
        if n.key == t || n.name == t || n.path.ends_with(t) {
            matches.push(TestStateTarget {
                label: format!("object {} ({})", n.name, n.key),
                path: n.path.clone(),
                current: flag_of(&n.teststate).to_string(),
                node_key: Some(n.key.clone()),
            });
        }
    }
    for u in &scan.usecases {
        if u.name == t || u.file.ends_with(t) {
            matches.push(TestStateTarget {
                label: format!("use case {}", u.name),
                path: u.file.clone(),
                current: flag_of(&u.teststate).to_string(),
                node_key: None,
            });
        }
    }
    for i in &scan.interfaces {
        if i.id == t || i.file.ends_with(t) {
            matches.push(TestStateTarget {
                label: format!("interface {}", i.id),
                path: i.file.clone(),
                current: flag_of(&i.teststate).to_string(),
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

/// Apply one teststate edit. Refusals guard the `block` contract: an existing
/// `block` survives everything except a human editing the file, and an
/// `include` under a blocked ANCESTOR refuses too, since it could never take
/// effect. Returns a one-line summary of what changed.
pub fn teststate_apply(scan: &Scan, target: &str, action: TestStateAction) -> Result<String, String> {
    let t = resolve_teststate_target(scan, target)?;
    if t.current == TS_BLOCK && action != TestStateAction::Block {
        return Err(format!(
            "{} is teststate: block (outside/vendor setup missing) — refusing to change it; \
             a human lifts a block by editing {}",
            t.label, t.path
        ));
    }
    if action == TestStateAction::Include {
        if let Some(key) = &t.node_key {
            let node = scan.nodes.iter().find(|n| n.key == *key);
            if let Some(node) = node {
                if let TestState::Omitted { value, by } = effective_teststate(node, &scan.nodes) {
                    if value == TS_BLOCK && by != *key {
                        let anc = scan.nodes.iter().find(|n| n.key == by);
                        return Err(format!(
                            "{} sits under blocked ancestor {} (teststate: block) — an include there \
                             can never take effect; a human lifts the block by editing {}",
                            t.label,
                            by,
                            anc.map(|a| a.path.clone()).unwrap_or_default()
                        ));
                    }
                }
            }
        }
    }
    let value = match action {
        TestStateAction::Omit => Some(TS_OMIT),
        TestStateAction::Include => Some(TS_INCLUDE),
        TestStateAction::Block => Some(TS_BLOCK),
        TestStateAction::Clear => None,
    };
    // Drop any legacy V1 key so old and new flags can never disagree.
    let _ = set_frontmatter_key(Path::new(&t.path), "test_loop", None);
    set_frontmatter_key(Path::new(&t.path), "teststate", value)?;
    Ok(format!(
        "{}: teststate {} (was {})",
        t.label,
        value.map(|v| format!("→ {}", v)).unwrap_or_else(|| "cleared".into()),
        if t.current.is_empty() { "unset" } else { &t.current }
    ))
}

/// Does this node text already carry a `# Long Description` heading (any level)?
fn has_long_description(content: &str) -> bool {
    content.lines().any(|l| {
        let t = l.trim_start();
        let hashes = t.chars().take_while(|c| *c == '#').count();
        (1..=3).contains(&hashes) && t[hashes..].trim_start().to_lowercase().starts_with("long description")
    })
}

/// One-time maintenance sweep (`iter stubdesc`): append `# Long Description\nTBD`
/// to every code node missing the section. Returns the stubbed file paths.
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
    fn dot_rule_decides_the_role() {
        // Valid forms, per the structureV2 naming rule.
        assert_eq!(role_of("my_thing.code.iter.md"), Some(Role::Code));
        assert_eq!(role_of("code.iter.md"), Some(Role::Code));
        assert_eq!(role_of(".code.iter.md"), Some(Role::Code));
        assert_eq!(role_of("my,super.duper.thing.code.iter.md"), Some(Role::Code));
        assert_eq!(role_of("README.main.iter.md"), Some(Role::Main));
        assert_eq!(role_of("x.testgroup.iter.md"), Some(Role::Testgroup));
        assert_eq!(role_of("x.usecase.iter.md"), Some(Role::Usecase));
        assert_eq!(role_of("x.bizreq.iter.md"), Some(Role::Bizreq));
        assert_eq!(role_of("x.techreq.iter.md"), Some(Role::Techreq));
        assert_eq!(role_of("x.interface.iter.md"), Some(Role::Interface));
        // Invalid: suffix-only matching is dead.
        assert_eq!(role_of("my_thing_code.iter.md"), None);
        assert_eq!(role_of("my-code.iter.md"), None);
        assert_eq!(role_of("barcode.iter.md"), None, "no suffix collision");
        assert_eq!(role_of("domain.iter.md"), None);
        assert_eq!(role_of("my_thing.Code.iter.md"), None, "case-sensitive");
        assert_eq!(role_of("marker.iter.md"), None, "V1 role name is dead");
        assert_eq!(role_of("code.md"), None, "must end .iter.md");
    }

    #[test]
    fn stems_strip_the_nodetype_segment() {
        assert_eq!(stem_of("mylib.code.iter.md"), "mylib");
        assert_eq!(stem_of("code.iter.md"), "");
        assert_eq!(stem_of(".code.iter.md"), "");
        assert_eq!(stem_of("a.b.code.iter.md"), "a.b");
        assert_eq!(stem_of("notes.iter.md"), "notes", "no role: whole stem");
    }

    #[test]
    fn front_parses_scalars_lists_and_children() {
        let text = r#"---
name: "My New Library"
description: A library to support some stuff  # trailing comment
level: container
owner: bespoke
teststate: inherit
globalscandirs: ["{topdir}/", "{topdir}/infra/"]
blocklist:
  - one
  - two
children:
  codedirs:   ["{topdir}/src/my_kafka_plugin/"]
  codenodes:  ["{codedirs}/**/consumer.code.iter.md", "{codedirs}/**/producer.code.iter.md"]
  bizreqs:    ["{thisfiledir}/bizreq.iter.md"]
  testgroups:
    - "{thisfiledir}/test/*.testgroup.iter.md"
---
# Body
prose here
"#;
        let f = parse_front(text);
        assert_eq!(f.scalar("name"), "My New Library");
        assert_eq!(f.scalar("description"), "A library to support some stuff");
        assert_eq!(f.scalar("owner"), "bespoke");
        assert_eq!(f.list("globalscandirs"), vec!["{topdir}/", "{topdir}/infra/"]);
        assert_eq!(f.list("blocklist"), vec!["one", "two"]);
        assert!(f.children_present);
        assert_eq!(f.child("codedirs").unwrap(), vec!["{topdir}/src/my_kafka_plugin/"]);
        assert_eq!(f.child("codenodes").unwrap().len(), 2);
        assert_eq!(f.child("testgroups").unwrap(), vec!["{thisfiledir}/test/*.testgroup.iter.md"]);
        assert!(f.child("inputs").is_none(), "absent sub-key = defaults apply");
        assert!(f.body.contains("prose here"));
    }

    #[test]
    fn front_coerces_single_string_to_list_and_tolerates_bullets() {
        let text = "---\nname: X\nchildren:\n  codenodes: {topdir}/a.code.iter.md\n  - testgroups: [\"t/*.testgroup.iter.md\"]\n---\nb";
        let f = parse_front(text);
        assert_eq!(f.child("codenodes").unwrap(), vec!["{topdir}/a.code.iter.md"], "string → 1-item list");
        assert_eq!(f.child("testgroups").unwrap(), vec!["t/*.testgroup.iter.md"], "bulleted sub-key tolerated");
    }

    #[test]
    fn legacy_test_loop_reads_as_teststate() {
        let f = parse_front("---\nname: X\ntest_loop: blocked\n---\nb");
        assert_eq!(f.teststate(), "block");
        let f = parse_front("---\nname: X\nteststate: omit\ntest_loop: include\n---\nb");
        assert_eq!(f.teststate(), "omit", "the V2 key wins");
    }

    /* ---------------- scan / DAG ---------------- */

    fn project_in(dir: &Path) -> Project {
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        Project::load(dir)
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-v2scan-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn explicit_links_build_the_dag_and_nesting_alone_links_nothing() {
        let dir = tmp("dag");
        write(&dir, "main.iter.md", "---\nprojectname: \"P\"\n---\nproject body\n");
        write(
            &dir,
            "core/core.code.iter.md",
            "---\nname: Core\nlevel: context\ndescription: d\nchildren:\n  codenodes: [\"{thisfiledir}/api/*.code.iter.md\"]\n---\nbody\n",
        );
        write(
            &dir,
            "core/api/api.code.iter.md",
            "---\nname: API\nlevel: container\ndescription: d\nchildren:\n  testgroups: [\"{thisfiledir}/test/*.testgroup.iter.md\"]\n---\nbody\n",
        );
        write(&dir, "core/api/test/api.testgroup.iter.md", "# tests\n");
        // Nested but NOT linked: no codenodes anywhere points at it → orphan.
        write(
            &dir,
            "core/api/stray/stray.code.iter.md",
            "---\nname: Stray\nlevel: component\ndescription: d\nchildren:\n  bizreqs: []\n---\nbody\n",
        );
        let project = project_in(&dir);
        let scan = scan(&project);
        let keys: Vec<&str> = scan.nodes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, vec!["core", "core/api"], "only linked nodes join: {:?}", keys);
        let api = scan.nodes.iter().find(|n| n.key == "core/api").unwrap();
        assert_eq!(api.parent, "core");
        assert_eq!(api.depth, 1);
        assert_eq!(api.testgroups.len(), 1, "declared fuzzy link picks up the file");
        assert!(scan.orphans.iter().any(|o| o.path.ends_with("stray.code.iter.md")), "{:?}", scan.orphans);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cycles_demote_the_edge_and_orphan_nothing_reachable() {
        let dir = tmp("cycle");
        write(&dir, "main.iter.md", "---\nprojectname: P\n---\nb\n");
        write(
            &dir,
            "a/a.code.iter.md",
            "---\nname: A\nlevel: context\ndescription: d\nchildren:\n  codenodes: [\"{topdir}/b/b.code.iter.md\"]\n---\nb\n",
        );
        write(
            &dir,
            "b/b.code.iter.md",
            "---\nname: B\nlevel: container\ndescription: d\nchildren:\n  codenodes: [\"{topdir}/a/a.code.iter.md\"]\n---\nb\n",
        );
        let project = project_in(&dir);
        let scan = scan(&project);
        assert_eq!(scan.nodes.len(), 2, "both stay in the DAG; only the edge demotes");
        assert!(scan.notes.iter().any(|n| n.contains("cycle demoted")), "{:?}", scan.notes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multi_parent_records_extra_parents() {
        let dir = tmp("multiparent");
        write(&dir, "main.iter.md", "---\nprojectname: P\n---\nb\n");
        for ctx in ["x", "y"] {
            write(
                &dir,
                &format!("{}/{}.code.iter.md", ctx, ctx),
                &format!(
                    "---\nname: {}\nlevel: context\ndescription: d\nchildren:\n  codenodes: [\"{{topdir}}/shared/shared.code.iter.md\"]\n---\nb\n",
                    ctx.to_uppercase()
                ),
            );
        }
        write(
            &dir,
            "shared/shared.code.iter.md",
            "---\nname: Shared\nlevel: component\ndescription: d\nchildren:\n  bizreqs: []\n---\nb\n",
        );
        let project = project_in(&dir);
        let scan = scan(&project);
        let shared = scan.nodes.iter().find(|n| n.name == "Shared").unwrap();
        assert_eq!(shared.parents.len(), 2, "both parents recorded: {:?}", shared.parents);
        assert_eq!(shared.parent, "x", "first (sorted) parent is primary");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interfaces_and_usecases_are_global_and_usecase_links_resolve_to_keys() {
        let dir = tmp("global");
        write(&dir, "main.iter.md", "---\nprojectname: P\n---\nb\n");
        write(
            &dir,
            "core/core.code.iter.md",
            "---\nname: Core\nlevel: context\ndescription: d\nchildren:\n  outputs: [\"{topdir}/interfaces/msg/msg.interface.iter.md\"]\n---\nb\n",
        );
        write(
            &dir,
            "interfaces/msg/msg.interface.iter.md",
            "---\nname: msg-api\nkind: request-reply\ndescription: d\nchildren:\n  testgroups: []\n---\ncontract\n",
        );
        write(
            &dir,
            "usecases/order.usecase.iter.md",
            "---\nname: Order\ndescription: d\nchildren:\n  codenodes: [\"{topdir}/core/core.code.iter.md\"]\n---\nstory\n",
        );
        let project = project_in(&dir);
        let scan = scan(&project);
        assert_eq!(scan.interfaces.len(), 1);
        assert_eq!(scan.interfaces[0].id, "msg-api");
        let core = &scan.nodes[0];
        assert_eq!(core.outputs, vec!["msg-api"], "output file resolves to the interface id");
        assert_eq!(scan.usecases.len(), 1);
        assert_eq!(scan.usecases[0].codenode_keys, vec!["core"]);
        assert!(scan.usecases[0].codenodes_declared);
        assert!(scan.orphans.is_empty(), "{:?}", scan.orphans);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /* ---------------- teststate ---------------- */

    fn mk(key: &str, parent: &str, ts: &str) -> Node {
        Node {
            key: key.into(),
            parent: parent.into(),
            parents: if parent.is_empty() { vec![] } else { vec![parent.into()] },
            teststate: ts.into(),
            ..Default::default()
        }
    }

    #[test]
    fn teststate_nearest_wins_and_block_beats_include() {
        let nodes = vec![
            mk("core", "", TS_OMIT),
            mk("core/api", "core", TS_INCLUDE),
            mk("core/db", "core", ""),
            mk("vendor", "", TS_BLOCK),
            mk("vendor/pay", "vendor", TS_INCLUDE),
            mk("other", "", ""),
        ];
        let eff = |key: &str| effective_teststate(nodes.iter().find(|n| n.key == key).unwrap(), &nodes);
        assert_eq!(eff("core/api"), TestState::Included, "include re-enters under an omitted ancestor");
        assert_eq!(eff("core/db"), TestState::Omitted { value: "omit".into(), by: "core".into() });
        assert_eq!(
            eff("vendor/pay"),
            TestState::Omitted { value: "block".into(), by: "vendor".into() },
            "block beats a descendant include"
        );
        assert_eq!(eff("other"), TestState::Included, "no flag anywhere = included");
    }

    #[test]
    fn multi_parent_included_via_any_chain() {
        // usecase-A-style chain says include, container-B chain says omit:
        // the shared node still tests (included via A's chain).
        let nodes = vec![
            mk("a", "", TS_INCLUDE),
            mk("b", "", TS_OMIT),
            {
                let mut n = mk("a/shared", "a", "");
                n.parents = vec!["a".into(), "b".into()];
                n
            },
        ];
        let eff = effective_teststate(nodes.iter().find(|n| n.key == "a/shared").unwrap(), &nodes);
        assert_eq!(eff, TestState::Included);
        // …but a block on EITHER chain wins.
        let mut blocked = nodes.clone();
        blocked[1].teststate = TS_BLOCK.into();
        let eff = effective_teststate(blocked.iter().find(|n| n.key == "a/shared").unwrap(), &blocked);
        assert_eq!(eff, TestState::Omitted { value: "block".into(), by: "b".into() });
    }

    #[test]
    fn own_teststate_for_global_objects() {
        assert_eq!(own_teststate("block", "f"), TestState::Omitted { value: "block".into(), by: "f".into() });
        assert_eq!(own_teststate("omit", "f"), TestState::Omitted { value: "omit".into(), by: "f".into() });
        assert_eq!(own_teststate("include", "f"), TestState::Included);
        assert_eq!(own_teststate("inherit", "f"), TestState::Included);
        assert_eq!(own_teststate("", "f"), TestState::Included);
    }

    #[test]
    fn set_frontmatter_key_preserves_everything_else() {
        let dir = tmp("tskey");
        let f = dir.join("x.code.iter.md");
        std::fs::write(&f, "---\nname: \"X\"\nlevel: container\nchildren:\n  testgroups: [\"test/*.testgroup.iter.md\"]\n---\n\n# Body\nstays verbatim\n").unwrap();
        set_frontmatter_key(&f, "teststate", Some("omit")).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("teststate: omit"), "{}", text);
        assert!(text.contains("name: \"X\"") && text.contains("stays verbatim"), "{}", text);
        assert!(text.contains("testgroups:"), "children survive: {}", text);
        set_frontmatter_key(&f, "teststate", Some("include")).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert_eq!(text.matches("teststate:").count(), 1, "{}", text);
        set_frontmatter_key(&f, "teststate", None).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains("teststate"), "{}", text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn teststate_apply_guards_the_block_contract() {
        let dir = tmp("tsapply");
        write(&dir, "main.iter.md", "---\nprojectname: P\n---\nb\n");
        write(
            &dir,
            "vendor/vendor.code.iter.md",
            "---\nname: Vendor\nlevel: context\ndescription: d\nteststate: block\nchildren:\n  codenodes: [\"{topdir}/vendor/pay/pay.code.iter.md\"]\n---\nb\n",
        );
        write(
            &dir,
            "vendor/pay/pay.code.iter.md",
            "---\nname: Pay\nlevel: component\ndescription: d\nchildren:\n  bizreqs: []\n---\nb\n",
        );
        write(
            &dir,
            "core/core.code.iter.md",
            "---\nname: Core\nlevel: context\ndescription: d\nteststate: omit\nchildren:\n  bizreqs: []\n---\nb\n",
        );
        let project = project_in(&dir);
        let s = scan(&project);
        for action in [TestStateAction::Include, TestStateAction::Omit, TestStateAction::Clear] {
            let err = teststate_apply(&s, "Vendor", action).unwrap_err();
            assert!(err.contains("block"), "{:?}: {}", action, err);
        }
        let err = teststate_apply(&s, "Pay", TestStateAction::Include).unwrap_err();
        assert!(err.contains("blocked ancestor"), "{}", err);
        assert!(teststate_apply(&s, "nope", TestStateAction::Omit).unwrap_err().contains("matches no"));
        let summary = teststate_apply(&s, "Core", TestStateAction::Include).unwrap();
        assert!(summary.contains("include"), "{}", summary);
        let text = std::fs::read_to_string(dir.join("core/core.code.iter.md")).unwrap();
        assert!(text.contains("teststate: include"), "{}", text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
