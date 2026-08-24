//! `iter migratev2` — the ONE-TIME V1 → V2 structure migration (structureV2.md).
//! Throwaway by design: pdy-dev is the only real project on iter, so this
//! favors clear reporting over generality. Deterministic work only — the
//! fuzzier link decisions (which the tool cannot prove) are left for Ingest
//! agents / a human, and named in the report.
//!
//! What it does, in order:
//! 1. Reads the V1 settings (.iter/projects.json + .engine/config.json's
//!    retired pathing keys) and writes the two V2 head files.
//! 2. The `level: project` marker becomes main.iter.md (name → projectname).
//! 3. Every other `*marker.iter.md` renames to `<stem>.code.iter.md`, its
//!    frontmatter rewritten: children links (testgroups/bizreqs/techreqs from
//!    the old scalar keys, codenodes from directory nesting, inputs/outputs
//!    from uses:/provides:), test_loop → teststate (blocked → block).
//! 4. Interfaces gain `name:` (from `interface:`), children.testgroups.
//! 5. Use-cases: participants → children.codenodes, testgroup → testgroups.
//! 6. bizreq/techreq/testgroup files gain the required frontmatter.
//! 7. Dot-rule renames for any remaining V1 suffix-style names.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::markers;

struct Mig {
    dry: bool,
    changes: Vec<String>,
}

impl Mig {
    fn log(&mut self, msg: String) {
        println!("  {}", msg);
        self.changes.push(msg);
    }
    fn write(&mut self, path: &Path, content: &str, what: &str) {
        self.log(format!("write {} ({})", path.display(), what));
        if !self.dry {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(path, content) {
                eprintln!("error: cannot write {}: {}", path.display(), e);
            }
        }
    }
    fn rename(&mut self, from: &Path, to: &Path) {
        self.log(format!("rename {} → {}", from.display(), to.display()));
        if !self.dry {
            if let Err(e) = std::fs::rename(from, to) {
                eprintln!("error: cannot rename {}: {}", from.display(), e);
            }
        }
    }
    fn remove(&mut self, path: &Path, why: &str) {
        self.log(format!("retire {} ({})", path.display(), why));
        if !self.dry {
            let bak = path.with_extension("v1bak");
            let _ = std::fs::rename(path, bak);
        }
    }
}

/// V1 role detection (the old case-insensitive suffix rule).
fn v1_role(filename: &str) -> Option<&'static str> {
    let lower = filename.to_ascii_lowercase();
    if !lower.ends_with(".iter.md") {
        return None;
    }
    for tag in ["marker", "bizreq", "techreq", "interface", "testgroup", "usecase"] {
        if lower.ends_with(&format!("{}.iter.md", tag)) {
            return Some(match tag {
                "marker" => "marker",
                "bizreq" => "bizreq",
                "techreq" => "techreq",
                "interface" => "interface",
                "testgroup" => "testgroup",
                _ => "usecase",
            });
        }
    }
    None
}

/// Flat V1 frontmatter (scalars + participants list + body).
fn v1_front(content: &str) -> (HashMap<String, String>, Vec<String>, String) {
    let mut map = HashMap::new();
    let mut participants = Vec::new();
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (map, participants, content.to_string());
    };
    let Some(end) = rest.find("\n---") else { return (map, participants, String::new()) };
    let body = rest[end + 4..].trim_start_matches('-').to_string();
    let mut in_participants = false;
    for line in rest[..end].lines() {
        let t = line.trim();
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

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', " "))
}

fn render_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().filter(|s| !s.trim().is_empty()).map(|s| quote(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// structureV2 encourages writing defaults out explicitly, so migrated files
/// ALWAYS carry the key — an unset V1 flag becomes `teststate: inherit`.
fn teststate_line(front: &HashMap<String, String>) -> String {
    match front.get("test_loop").map(|s| s.trim()) {
        Some("blocked") => "teststate: block\n".into(),
        Some(v @ ("omit" | "include")) => format!("teststate: {}\n", v),
        _ => "teststate: inherit\n".into(),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".iter") {
                continue;
            }
            walk(&path, out);
        } else if name.ends_with(".iter.md") {
            out.push(path);
        }
    }
}

pub fn run(project_root: &Path, dry: bool) -> i32 {
    let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    let mut mig = Mig { dry, changes: Vec::new() };
    println!(
        "migratev2: {} ({})",
        root.display(),
        if dry { "DRY RUN — nothing written" } else { "writing changes" }
    );

    // 1. V1 settings.
    let v1_projects: serde_json::Value = std::fs::read_to_string(root.join(".iter/projects.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::json!({}));
    let v1_engine: serde_json::Value = std::fs::read_to_string(root.join(".iter/.engine/config.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::json!({}));
    let gs = &v1_engine["globalsettings"];
    let code_root_raw = gs["code_root"].as_str().unwrap_or(".").trim();
    let topdir = {
        // V1 code_root commonly used `~` (pdy-dev: "~/dev/pdy-dev") — expand it
        // before deciding absolute vs engine-home-relative.
        let mut raw_s = code_root_raw.to_string();
        if let Some(rest) = raw_s.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                raw_s = format!("{}/{}", home.to_string_lossy(), rest);
            }
        }
        let p = if raw_s.is_empty() || raw_s == "." {
            root.clone()
        } else {
            let raw = PathBuf::from(&raw_s);
            if raw.is_absolute() { raw } else { root.join(raw) }
        };
        p.canonicalize().unwrap_or(p)
    };
    // topdir written relative to {thisfiledir} (.iter/) when it is an ancestor.
    let topdir_setting = {
        let iter_dir = root.join(".iter");
        let mut hops = 1; // .iter → root
        let mut cur = root.clone();
        let mut found = cur == topdir;
        while !found {
            let Some(parent) = cur.parent() else { break };
            cur = parent.to_path_buf();
            hops += 1;
            found = cur == topdir;
        }
        if found {
            format!("{{thisfiledir}}/{}", "../".repeat(hops))
        } else {
            let _ = iter_dir;
            format!("{}/", topdir.display())
        }
    };

    // 2. Gather V1 files under the old scan roots (default: the topdir).
    let mut files: Vec<PathBuf> = Vec::new();
    let scan_roots: Vec<PathBuf> = v1_projects["scan_roots"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| {
                    let mut p = s.to_string();
                    if let Some(rest) = p.strip_prefix("~/") {
                        if let Some(home) = std::env::var_os("HOME") {
                            p = format!("{}/{}", home.to_string_lossy(), rest);
                        }
                    }
                    let pb = PathBuf::from(&p);
                    if pb.is_absolute() { pb } else { root.join(pb) }
                })
                .collect()
        })
        .unwrap_or_else(|| vec![topdir.clone()]);
    for r in &scan_roots {
        walk(r, &mut files);
    }
    files.sort();
    files.dedup();

    // Classify V1 files.
    struct V1Marker {
        path: PathBuf,
        dir: PathBuf,
        front: HashMap<String, String>,
        body: String,
    }
    let mut project_marker: Option<V1Marker> = None;
    let mut v1_markers: Vec<V1Marker> = Vec::new();
    let mut iface_id_to_path: HashMap<String, PathBuf> = HashMap::new();
    let mut others: Vec<(PathBuf, &'static str)> = Vec::new();
    for path in &files {
        let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        // Already-valid V2 names with V2 roles pass through mostly untouched;
        // classification below uses the V1 rule to find what must change.
        let Some(role) = v1_role(&fname) else { continue };
        let Ok(content) = std::fs::read_to_string(path) else { continue };
        let (front, _participants, body) = v1_front(&content);
        match role {
            "marker" => {
                let m = V1Marker {
                    path: path.clone(),
                    dir: path.parent().map(PathBuf::from).unwrap_or_default(),
                    front,
                    body,
                };
                if m.front.get("level").map(|l| l.trim() == "project").unwrap_or(false)
                    && project_marker.is_none()
                {
                    project_marker = Some(m);
                } else {
                    v1_markers.push(m);
                }
            }
            "interface" => {
                if let Some(id) = front.get("interface").filter(|s| !s.trim().is_empty()) {
                    iface_id_to_path.insert(id.trim().to_string(), path.clone());
                }
                others.push((path.clone(), "interface"));
            }
            r => others.push((path.clone(), if r == "marker" { "marker" } else { r })),
        }
    }

    // Directory-nesting map for codenodes + participant-key resolution: V1
    // ancestry WAS the directory tree, so the deterministic links come from it.
    let marker_dirs: Vec<(PathBuf, PathBuf)> = v1_markers
        .iter()
        .map(|m| (m.dir.clone(), m.path.clone()))
        .collect();
    let new_code_path = |old: &Path| -> PathBuf {
        let fname = old.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let stem = fname
            .strip_suffix(".iter.md")
            .unwrap_or(&fname)
            .trim_end_matches("marker")
            .trim_end_matches(['.', '-', '_'])
            .to_string();
        let stem = if stem.is_empty() {
            old.parent()
                .and_then(|d| d.file_name())
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|| "node".into())
        } else {
            stem
        };
        old.with_file_name(format!("{}.code.iter.md", stem))
    };
    let direct_children = |dir: &Path| -> Vec<PathBuf> {
        // Markers whose dir strictly descends from `dir` with no marker dir between.
        let mut out = Vec::new();
        for (cdir, cpath) in &marker_dirs {
            if cdir == dir || !cdir.starts_with(dir) {
                continue;
            }
            let intermediate = marker_dirs.iter().any(|(odir, _)| {
                odir != dir && odir != cdir && odir.starts_with(dir) && cdir.starts_with(odir)
            });
            if !intermediate {
                out.push(cpath.clone());
            }
        }
        out.sort();
        out
    };
    let key_to_code_path = |key: &str| -> Option<PathBuf> {
        // V1 participant keys were dirs relative to the topdir ("." = root).
        if key == "." || key.is_empty() {
            return None;
        }
        let dir = topdir.join(key);
        marker_dirs.iter().find(|(d, _)| *d == dir).map(|(_, p)| new_code_path(p))
    };

    // 3. Head files.
    println!("\nhead files:");
    let (projectname, projectdescription, main_body) = match &project_marker {
        Some(m) => (
            m.front.get("name").cloned().unwrap_or_else(|| {
                v1_projects["project_name"].as_str().unwrap_or("project").to_string()
            }),
            m.front.get("description").cloned().unwrap_or_default(),
            m.body.clone(),
        ),
        None => (
            v1_projects["project_name"].as_str().unwrap_or("project").to_string(),
            String::new(),
            String::new(),
        ),
    };
    let rel_to_topdir = |p: &str| -> String {
        p.replace("{codepath}", "{topdir}")
    };
    let interfacedir = rel_to_topdir(gs["interface_default_path"].as_str().unwrap_or("{topdir}/interfaces/"));
    let usecasedir = rel_to_topdir(gs["usecase_default_path"].as_str().unwrap_or("{topdir}/usecases/"));
    let mut contextfiles: Vec<String> = Vec::new();
    for k in ["global_bizreq_path", "global_techreq_path"] {
        if let Some(v) = gs[k].as_str() {
            contextfiles.push(rel_to_topdir(v));
        }
    }
    if contextfiles.is_empty() {
        contextfiles.push("{topdir}/reqs/*.iter.md".into());
    }
    // Root children: the project marker's direct children by V1 nesting.
    let root_children: Vec<String> = match &project_marker {
        Some(m) => direct_children(&m.dir)
            .iter()
            .map(|p| new_code_path(p))
            .map(|p| {
                p.strip_prefix(&topdir)
                    .map(|r| format!("{{topdir}}/{}", r.display()))
                    .unwrap_or_else(|_| p.to_string_lossy().into_owned())
            })
            .collect(),
        None => Vec::new(),
    };
    let mainfile = topdir.join("main.iter.md");
    let main_content = format!(
        "---\nprojectname: {}\nprojectdescription: {}\nglobalscandirs: [\"{{topdir}}/\"]\nglobalinterfacedir: {}\nglobalusecasedir: {}\nglobalcontextfiles: {}\nchildren:\n  codenodes: {}\n---\n{}",
        quote(&projectname),
        quote(&projectdescription),
        quote(&interfacedir),
        quote(&usecasedir),
        render_list(&contextfiles),
        render_list(&root_children),
        if main_body.trim().is_empty() {
            format!("\n# {}\n", projectname)
        } else {
            format!("\n{}\n", main_body.trim())
        }
    );
    mig.write(&mainfile, &main_content, "V2 project head (from the level: project marker)");
    if let Some(m) = &project_marker {
        mig.remove(&m.path, "became main.iter.md");
    }
    let server_cfg = format!(
        "{{\n  \"mainfile\": \"{{topdir}}/main.iter.md\",\n  \"iterglob\": \"**/*.iter.md\",\n  \"topdir\": \"{}\",\n  \"url_slug\": \"{}\",\n  \"default_context\": {}\n}}\n",
        topdir_setting,
        v1_projects["url_slug"].as_str().unwrap_or(""),
        serde_json::to_string(
            &v1_projects["default_context"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![
                    serde_json::json!("{marker}"),
                    serde_json::json!("{ancestor_markers}"),
                    serde_json::json!("{interfaces}")
                ])
        )
        .unwrap_or_else(|_| "[]".into())
    );
    mig.write(&crate::project::config_path(&root), &server_cfg, "V2 server config");
    if root.join(".iter/projects.json").exists() {
        mig.remove(&root.join(".iter/projects.json"), "retired by config.iter.json + main.iter.md");
    }

    // 4. Markers → code nodes.
    println!("\ncode nodes:");
    for m in &v1_markers {
        let new_path = new_code_path(&m.path);
        let name = m.front.get("name").cloned().unwrap_or_default();
        let level = match m.front.get("level").map(|s| s.trim()) {
            Some("project") => "context".to_string(),
            Some(l) if !l.is_empty() => l.to_string(),
            _ => "component".to_string(),
        };
        // The FULL canonical children mapping, every sub-key written out (the
        // spec's "write the defaults explicitly" encouragement) — empty lists
        // where V1 declared nothing, so the shape matches the spec example.
        let codenodes_entries: Vec<String> = direct_children(&m.dir)
            .iter()
            .map(|k| new_code_path(k))
            .filter_map(|k| {
                k.strip_prefix(&m.dir).ok().map(|r| format!("{{thisfiledir}}/{}", r.display()))
            })
            .collect();
        let mut iface_entries = |v1key: &str| -> Vec<String> {
            let Some(raw) = m.front.get(v1key) else { return Vec::new() };
            let ids: Vec<&str> = raw
                .trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let mut entries = Vec::new();
            for id in &ids {
                match iface_id_to_path.get(*id) {
                    Some(p) => entries.push(
                        p.strip_prefix(&topdir)
                            .map(|r| format!("{{topdir}}/{}", r.display()))
                            .unwrap_or_else(|_| p.to_string_lossy().into_owned()),
                    ),
                    None => mig.log(format!(
                        "NOTE {}: {} id \"{}\" matches no interface file — left out; Ingest should relink",
                        m.path.display(),
                        v1key,
                        id
                    )),
                }
            }
            entries
        };
        let inputs_entries = iface_entries("uses");
        let outputs_entries = iface_entries("provides");
        let req_entry = |v1key: &str, default_glob: &str| -> Vec<String> {
            match m.front.get(v1key).filter(|v| !v.trim().is_empty()) {
                Some(v) => vec![format!("{{thisfiledir}}/{}", v.trim())],
                None => vec![default_glob.to_string()],
            }
        };
        let tg_entries = match m.front.get("testgroup").filter(|v| !v.trim().is_empty() && v.trim() != "none") {
            Some(v) => vec![format!("{{thisfiledir}}/{}", v.trim())],
            None => vec!["{thisfiledir}/test/*.testgroup.iter.md".to_string()],
        };
        let mut children = String::from("children:\n");
        for (key, entries) in [
            ("codedirs:  ", vec!["{thisfiledir}/".to_string()]),
            ("codenodes: ", codenodes_entries),
            ("inputs:    ", inputs_entries),
            ("outputs:   ", outputs_entries),
            ("bizreqs:   ", req_entry("bizreq", "{thisfiledir}/*.bizreq.iter.md")),
            ("techreqs:  ", req_entry("techreq", "{thisfiledir}/*.techreq.iter.md")),
            ("testgroups:", tg_entries),
        ] {
            children.push_str(&format!("  {} {}\n", key, render_list(&entries)));
        }
        let content = format!(
            "---\nname: {}\nlevel: {}\ndescription: {}\nowner: bespoke\n{}{}---\n{}",
            quote(&name),
            level,
            quote(m.front.get("description").map(String::as_str).unwrap_or("")),
            teststate_line(&m.front),
            children,
            if m.body.trim().is_empty() { "\n".to_string() } else { format!("\n{}\n", m.body.trim()) }
        );
        mig.write(&new_path, &content, "V2 code node");
        if new_path != m.path {
            mig.remove(&m.path, "replaced by the .code.iter.md file");
        }
    }

    // 5. Interfaces, use-cases, testgroups, reqs, and dot-rule renames.
    println!("\nglobal objects and support files:");
    for (path, role) in &others {
        let Ok(content) = std::fs::read_to_string(path) else { continue };
        let (front, participants, body) = v1_front(&content);
        let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        // Dot rule: insert the missing dot when the V1 name ran the nodetype
        // into the prefix ("xusecase.iter.md" → "x.usecase.iter.md").
        let fixed_name = if markers::role_of(&fname).is_none() {
            let stem = fname.strip_suffix(&format!("{}.iter.md", role)).unwrap_or("");
            let stem = stem.trim_end_matches(['.', '-', '_']);
            if stem.is_empty() {
                format!("{}.iter.md", role)
            } else {
                format!("{}.{}.iter.md", stem, role)
            }
        } else {
            fname.clone()
        };
        let new_path = path.with_file_name(&fixed_name);
        match *role {
            "interface" => {
                let id = front
                    .get("interface")
                    .or(front.get("name"))
                    .cloned()
                    .unwrap_or_else(|| markers::stem_of(&fixed_name));
                let content = format!(
                    "---\nname: {}\nkind: {}\ndescription: {}\nowner: bespoke\n{}children:\n  bizreqs:    [\"{{thisfiledir}}/{{thisfilestem}}/*.bizreq.iter.md\"]\n  techreqs:   [\"{{thisfiledir}}/{{thisfilestem}}/*.techreq.iter.md\"]\n  testgroups: {}\n---\n\n{}\n",
                    quote(&id),
                    front.get("kind").map(String::as_str).unwrap_or(""),
                    quote(front.get("description").map(String::as_str).unwrap_or("")),
                    teststate_line(&front),
                    match front.get("testgroup").filter(|v| !v.trim().is_empty() && v.trim() != "none") {
                        Some(v) => render_list(&[format!("{{thisfiledir}}/{}", v.trim())]),
                        None => "[\"{thisfiledir}/{thisfilestem}/*.testgroup.iter.md\"]".into(),
                    },
                    body.trim()
                );
                mig.write(&new_path, &content, "V2 interface");
                if new_path != *path {
                    mig.remove(path, "dot-rule rename");
                }
            }
            "usecase" => {
                let mut codenodes: Vec<String> = Vec::new();
                for p in &participants {
                    let key = p.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                    match key_to_code_path(key.trim()) {
                        Some(cp) => codenodes.push(
                            cp.strip_prefix(&topdir)
                                .map(|r| format!("{{topdir}}/{}", r.display()))
                                .unwrap_or_else(|_| cp.to_string_lossy().into_owned()),
                        ),
                        None => {
                            if key.trim() != "." && !key.trim().is_empty() {
                                mig.log(format!(
                                    "NOTE {}: participant \"{}\" resolved to no marker dir — left out; Ingest should relink",
                                    path.display(),
                                    p
                                ));
                            }
                        }
                    }
                }
                codenodes.dedup();
                let content = format!(
                    "---\nname: {}\ndescription: {}\n{}children:\n  codenodes:  {}\n  testgroups: {}\n---\n\n{}\n",
                    quote(front.get("name").map(String::as_str).unwrap_or("")),
                    quote(front.get("description").map(String::as_str).unwrap_or("")),
                    teststate_line(&front),
                    render_list(&codenodes),
                    match front.get("testgroup").filter(|v| !v.trim().is_empty() && v.trim() != "none") {
                        Some(v) => render_list(&[format!("{{thisfiledir}}/{}", v.trim())]),
                        None => "[\"{thisfiledir}/{thisfilestem}/*.testgroup.iter.md\"]".into(),
                    },
                    body.trim()
                );
                mig.write(&new_path, &content, "V2 use-case (participants → codenodes)");
                if new_path != *path {
                    mig.remove(path, "dot-rule rename");
                }
            }
            "testgroup" | "bizreq" | "techreq" => {
                let has_front = content.trim_start().starts_with("---");
                let needs_rename = new_path != *path;
                if has_front && !needs_rename {
                    continue; // already V2-ish; validate flags anything left
                }
                let new_content = if has_front {
                    content.clone()
                } else {
                    let stem = markers::stem_of(&fixed_name);
                    let label = if stem.is_empty() {
                        path.parent()
                            .and_then(|d| d.file_name())
                            .map(|d| d.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    } else {
                        stem
                    };
                    let children = match *role {
                        "testgroup" => "  testpaths: [\"{thisfiledir}/*.sh\"]",
                        _ => "  reqpaths: []",
                    };
                    format!(
                        "---\nname: {}\ndescription: {}\nchildren:\n{}\n---\n\n{}",
                        quote(&format!("{} {}", label, role)),
                        quote(&format!("{} for {}", role, label)),
                        children,
                        content
                    )
                };
                mig.write(&new_path, &new_content, &format!("V2 {} (frontmatter added)", role));
                if needs_rename {
                    mig.remove(path, "dot-rule rename");
                }
            }
            _ => {}
        }
    }

    println!("\nmigratev2: {} change(s){}", mig.changes.len(), if dry { " (dry run)" } else { "" });
    println!("next: run `iter validate --project {}` and `iter markers --project {}` to inspect the DAG and the Orphanage", root.display(), root.display());
    0
}
