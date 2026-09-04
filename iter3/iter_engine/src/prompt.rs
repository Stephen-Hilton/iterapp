//! Prompt assembly, V2-faithful (src/scheduler.rs build_turns in the V2 crate):
//! spin-up = agent body + shared rules + capability index + project head files
//! + source instructions + the work item block + previous attempt + context
//! files; then the turn sequence prework(prose) → mainwork → postwork(prose) →
//! self-check, all in ONE claude session (--resume).  Nothing here is inlined
//! file content: like V2, the agent is told which files to read.

use iter_core::{Project, WorkItem};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CRITREVIEW_MAX_ROUNDS: u32 = 3;
const PREV_OUTPUT_TAIL_CHARS: usize = 4000;

/// The tooling rows the engine needs, by kind.
#[derive(Default, Clone)]
pub struct Tooling {
    pub shared: String,
    /// name -> desc
    pub capabilities: BTreeMap<String, String>,
    /// name -> body (user | agent | error)
    pub sources: BTreeMap<String, String>,
    /// name -> body (prose pre/postwork steps)
    pub prepost: BTreeMap<String, String>,
}

impl Tooling {
    pub fn from_rows(rows: &[Value]) -> Self {
        let mut t = Tooling::default();
        let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        for r in rows {
            match s(r, "kind").as_str() {
                "shared" => t.shared = s(r, "body"),
                "capability" => {
                    t.capabilities.insert(s(r, "name"), s(r, "desc"));
                }
                "source" => {
                    t.sources.insert(s(r, "name"), s(r, "body"));
                }
                "prepost" => {
                    t.prepost.insert(s(r, "name"), s(r, "body"));
                }
                _ => {}
            }
        }
        t
    }
}

/// The project head as read from main.iter.md's frontmatter (structureV2).
#[derive(Default, Clone, Debug)]
pub struct Head {
    pub mainfile: PathBuf,
    pub context_files: Vec<PathBuf>,
    pub interface_dir: String,
    pub usecase_dir: String,
}

fn frontmatter(text: &str) -> BTreeMap<String, String> {
    let mut fm = BTreeMap::new();
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some((k, v)) = line.split_once(':') {
                    fm.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }
    }
    fm
}

/// A YAML-ish list value: ["a", "b"] or a bare scalar.
fn list_value(v: &str) -> Vec<String> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        inner.split(',').map(|x| x.trim().trim_matches('"').trim_matches('\'').to_string()).filter(|x| !x.is_empty()).collect()
    } else if v.is_empty() {
        vec![]
    } else {
        vec![v.trim_matches('"').to_string()]
    }
}

pub fn expand_topdir_token(pattern: &str, topdir: &Path) -> String {
    let top = topdir.to_string_lossy().trim_end_matches('/').to_string();
    let mut p = pattern.replace("{topdir}/", &format!("{top}/")).replace("{topdir}", &top);
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            p = format!("{home}/{rest}");
        }
    }
    while p.contains("//") {
        p = p.replace("//", "/");
    }
    p
}

/// Resolve one pattern (absolute, {topdir}-relative, relative, glob) to files.
fn resolve_files(pattern: &str, topdir: &Path) -> Vec<PathBuf> {
    let p = expand_topdir_token(pattern, topdir);
    let abs = if Path::new(&p).is_absolute() { p } else { topdir.join(&p).to_string_lossy().into_owned() };
    let mut out = Vec::new();
    if abs.contains('*') || abs.contains('?') || abs.contains('[') {
        if let Ok(paths) = glob::glob(&abs) {
            for e in paths.flatten() {
                if e.is_file() {
                    out.push(e);
                }
            }
        }
    } else {
        let path = PathBuf::from(&abs);
        if path.is_file() {
            out.push(path);
        }
    }
    out
}

pub fn read_head(project: &Project, topdir: &Path) -> Head {
    let mainfile = PathBuf::from(expand_topdir_token(&project.mainfile, topdir));
    let mut head = Head { mainfile: mainfile.clone(), ..Default::default() };
    let Ok(text) = std::fs::read_to_string(&mainfile) else { return head };
    let fm = frontmatter(&text);
    head.context_files.push(mainfile);
    for pat in list_value(fm.get("globalcontextfiles").map(String::as_str).unwrap_or("")) {
        head.context_files.extend(resolve_files(&pat, topdir));
    }
    head.context_files.dedup();
    head.interface_dir = expand_topdir_token(fm.get("globalinterfacedir").map(String::as_str).unwrap_or("{topdir}/interfaces/"), topdir);
    head.usecase_dir = expand_topdir_token(fm.get("globalusecasedir").map(String::as_str).unwrap_or("{topdir}/usecases/"), topdir);
    head
}

/// All *.iter.md files directly inside a directory.
fn markers_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.file_name().map(|n| n.to_string_lossy().ends_with(".iter.md")).unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// {marker}: the *.iter.md files of the nearest directory at or above the
/// codepath that has any; {ancestor_markers}: the same for every directory
/// above that one up to (excluding) topdir.
pub fn marker_chain(codepath: &Path, topdir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dir = if codepath.is_dir() { codepath.to_path_buf() } else { codepath.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
    let top = topdir.canonicalize().unwrap_or(topdir.to_path_buf());
    let mut marker = Vec::new();
    let mut ancestors = Vec::new();
    let mut found = false;
    loop {
        let here = dir.canonicalize().unwrap_or(dir.clone());
        if !here.starts_with(&top) {
            break;
        }
        let ms = markers_in(&here);
        if !ms.is_empty() {
            if !found {
                marker = ms;
                found = true;
            } else {
                ancestors.extend(ms);
            }
        }
        if here == top {
            break; // the root's markers count (main.iter.md is filtered out later as the head)
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    (marker, ancestors)
}

/// The item's context patterns (or the project defaults) resolved to files.
pub fn resolve_context(item: &WorkItem, project: &Project, codepath: &Path, topdir: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let patterns: &Vec<String> = if item.context.is_empty() { &project.default_context } else { &item.context };
    let (marker, ancestors) = marker_chain(codepath, topdir);
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for pat in patterns {
        match pat.trim() {
            "{marker}" => files.extend(marker.iter().cloned()),
            "{ancestor_markers}" => files.extend(ancestors.iter().cloned()),
            "{interfaces}" => {}
            p => {
                let p = p.replace("{codepath}", &codepath.to_string_lossy());
                let hits = resolve_files(&p, topdir);
                if hits.is_empty() {
                    warnings.push(format!("context pattern matched nothing: {pat}"));
                }
                files.extend(hits);
            }
        }
    }
    files.sort();
    files.dedup();
    (files, warnings)
}

/// "user" | "agent:<type>" | "error" | anything else (a username) -> user.
pub fn source_instructions(tooling: &Tooling, requestedby: &str, createdby_agent: &str) -> Option<String> {
    let rb = requestedby.trim();
    let (key, agent_type) = if rb == "error" {
        ("error", String::new())
    } else if let Some(rest) = rb.strip_prefix("agent") {
        let t = rest.trim_start_matches(':').trim().to_string();
        ("agent", if t.is_empty() { createdby_agent.to_string() } else { t })
    } else if !createdby_agent.is_empty() && rb.is_empty() {
        ("agent", createdby_agent.to_string())
    } else {
        ("user", String::new())
    };
    let text = tooling.sources.get(key)?;
    Some(if key == "agent" { text.replace("{type}", &agent_type) } else { text.clone() })
}

pub struct SpinupInput<'a> {
    pub agent_body: &'a str,
    pub tooling: &'a Tooling,
    pub head: &'a Head,
    pub project: &'a Project,
    pub item: &'a WorkItem,
    pub codepath: &'a Path,
    pub topdir: &'a Path,
    pub requestedby: &'a str,
    pub createdby_agent: &'a str,
    pub last_response_tail: &'a str,
    pub close_gate_paragraph: &'a str,
}

/// The spin-up text prepended to the first turn.  Assembly order is load-
/// bearing (prompt cache): everything up to "# Source instructions" is
/// byte-identical for every item an agent type runs.
pub fn spinup(inp: &SpinupInput) -> (String, Vec<PathBuf>, Vec<String>) {
    let mut s = String::new();
    s.push_str(inp.agent_body.trim_end());
    if !inp.tooling.shared.trim().is_empty() {
        s.push_str("\n\n# Shared instructions (all agents)\n");
        s.push_str(&inp.tooling.shared.replace("{critreview_max_rounds}", &CRITREVIEW_MAX_ROUNDS.to_string()));
    }
    if !inp.tooling.capabilities.is_empty() {
        s.push_str("\n\n# Capabilities (read the full doc when you need one: `iter capability <name>`)\n");
        for (name, desc) in &inp.tooling.capabilities {
            s.push_str(&format!("- {name}: {desc}\n"));
        }
    }
    if !inp.head.context_files.is_empty() {
        s.push_str("\n\n# Project context ($ITER_MAINFILE + globalcontextfiles)\nThe project definition and global requirements — read what applies before starting:\n");
        for f in &inp.head.context_files {
            s.push_str(&format!("- {}\n", f.display()));
        }
    }
    s.push_str(inp.close_gate_paragraph);
    // ---- everything below varies per work item ----
    if let Some(src) = source_instructions(inp.tooling, inp.requestedby, inp.createdby_agent) {
        s.push_str("\n\n# Source instructions\n");
        s.push_str(&src);
    }
    s.push_str(&format!(
        "\n\n# Work item\nTitle: {}\nWork item id: {}\nCodepath (your working directory and lock scope): {}\nPriority: P{}\n",
        inp.item.name,
        inp.item.id,
        inp.codepath.display(),
        inp.item.priority
    ));
    if !inp.item.lasterror.is_empty() {
        s.push_str(&format!("\n# Previous attempt\nThis work item ran before and did not complete. Last error: {}\n", inp.item.lasterror));
        let out = inp.last_response_tail.trim();
        if !out.is_empty() {
            let start = out.char_indices().rev().nth(PREV_OUTPUT_TAIL_CHARS.saturating_sub(1)).map(|(i, _)| i).unwrap_or(0);
            s.push_str(&format!("Partial output of the previous attempt{}:\n{}\n", if start > 0 { " (tail)" } else { "" }, &out[start..]));
        }
    }
    let (files, warnings) = resolve_context(inp.item, inp.project, inp.codepath, inp.topdir);
    let item_files: Vec<PathBuf> = files.into_iter().filter(|f| !inp.head.context_files.contains(f)).collect();
    if !item_files.is_empty() {
        s.push_str("\n# Context files\nRead each of these before starting:\n");
        for f in &item_files {
            s.push_str(&format!("- {}\n", f.display()));
        }
    }
    (s, item_files, warnings)
}

/// Mainwork prompt: the request, preceded by an answered question when the
/// item went through the `question` state (the answer outranks the request).
pub fn mainwork_prompt(request: &str, answered: Option<(String, String)>) -> String {
    match answered {
        Some((q, a)) if !q.trim().is_empty() && !a.trim().is_empty() => format!(
            "# A question on this work item was answered\n\n\
             Before this run, work on this item stopped to ask a human a question.\n\
             The question and the answer are below — the answer is a decision, and it \
             outranks any assumption in the request that follows.\n\n\
             ## Asked\n\n{}\n\n## Answer\n\n{}\n\n\
             Proceed on that answer. If it is ambiguous, or acting on it raises a NEW \
             decision only a human can make, ask again with `iter ask` rather than \
             guessing.\n\n---\n\n{}",
            q.trim(),
            a.trim(),
            request
        ),
        _ => request.to_string(),
    }
}

/// The final turn of every run.
pub fn selfcheck_prompt(agent_body: &str, shared: &str) -> String {
    let shared_section = if shared.trim().is_empty() { String::new() } else { format!("\n\n# Shared instructions (all agents)\n{shared}") };
    format!(
        "Final check: re-read your agent definition below and confirm every instruction was \
         completed for this work item. Report anything unfinished or skipped (each on its own \
         line starting with \"NOT DONE:\"), or confirm all done.\n\n---\n{}{}",
        agent_body, shared_section
    )
}

/// The latest answered question widget the agent has not yet been shown
/// (`surfaced` is set on the row once a mainwork turn carried the answer):
/// (row order, question text, answer text).
pub fn answered_question(details: &[Value]) -> Option<(i64, String, String)> {
    let order = |d: &Value| d.get("order").and_then(|o| o.as_i64()).unwrap_or(0);
    let q = details
        .iter()
        .filter(|d| d.get("key").and_then(|k| k.as_str()) == Some("question"))
        .filter(|d| !d.get("value").and_then(|v| v.get("surfaced")).and_then(|b| b.as_bool()).unwrap_or(false))
        .max_by_key(|d| order(d))?;
    let w = q.get("value")?;
    let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("");
    let detail = w.get("detail").and_then(|t| t.as_str()).unwrap_or("");
    let question = if detail.trim().is_empty() { title.to_string() } else { detail.to_string() };
    let mut answers = Vec::new();
    for f in w.get("fields").and_then(|f| f.as_array()).cloned().unwrap_or_default() {
        let label = f.get("label").or(f.get("key")).and_then(|x| x.as_str()).unwrap_or("");
        let v = f.get("value").cloned().unwrap_or(Value::Null);
        let text = match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        };
        if !text.trim().is_empty() {
            answers.push(if label.is_empty() { text } else { format!("{label}: {text}") });
        }
    }
    if answers.is_empty() {
        return None;
    }
    Some((order(q), question, answers.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_lists_and_head() {
        let dir = std::env::temp_dir().join(format!("iter3-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("core/repos/x/deep")).unwrap();
        std::fs::create_dir_all(dir.join("reqs")).unwrap();
        std::fs::write(dir.join("main.iter.md"), "---\nprojectname: \"P\"\nglobalcontextfiles: [\"{topdir}/reqs/_index.md\", \"{topdir}/reqs/*.iter.md\"]\n---\nbody").unwrap();
        std::fs::write(dir.join("reqs/_index.md"), "i").unwrap();
        std::fs::write(dir.join("reqs/techreq.iter.md"), "t").unwrap();
        std::fs::write(dir.join("core/core.code.iter.md"), "c").unwrap();
        std::fs::write(dir.join("core/repos/x/x.code.iter.md"), "x").unwrap();
        std::fs::write(dir.join("core/repos/x/x.techreq.iter.md"), "xt").unwrap();
        let project = Project { mainfile: "{topdir}/main.iter.md".into(), default_context: vec!["{marker}".into(), "{ancestor_markers}".into()], ..Default::default() };
        let head = read_head(&project, &dir);
        assert_eq!(head.context_files.len(), 3, "{:?}", head.context_files);
        let (marker, anc) = marker_chain(&dir.join("core/repos/x/deep"), &dir);
        assert_eq!(marker.len(), 2, "{marker:?}");
        assert_eq!(anc.len(), 2, "{anc:?}"); // core.code.iter.md + the root's main.iter.md
        let item = WorkItem { context: vec![], ..Default::default() };
        let (files, warn) = resolve_context(&item, &project, &dir.join("core/repos/x/deep"), &dir);
        assert_eq!(files.len(), 4);
        assert!(warn.is_empty());
        let item2 = WorkItem { context: vec!["{topdir}/reqs/nope.md".into(), "{codepath}/../x.code.iter.md".into()], ..Default::default() };
        let (files2, warn2) = resolve_context(&item2, &project, &dir.join("core/repos/x/deep"), &dir);
        assert_eq!(files2.len(), 1);
        assert_eq!(warn2.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_and_answered_question() {
        let mut t = Tooling::default();
        t.sources.insert("user".into(), "U".into());
        t.sources.insert("agent".into(), "A {type}".into());
        assert_eq!(source_instructions(&t, "user", "").as_deref(), Some("U"));
        assert_eq!(source_instructions(&t, "agent:plan", "").as_deref(), Some("A plan"));
        assert_eq!(source_instructions(&t, "stephen", "").as_deref(), Some("U"));
        assert_eq!(source_instructions(&t, "", "code").as_deref(), Some("A code"));
        let details = vec![
            serde_json::json!({"order":0,"key":"request","value":"r"}),
            serde_json::json!({"order":1,"key":"question","value":{"title":"Which?","detail":"","fields":[{"key":"answer","label":"Answer","type":"text","value":"B"}]}}),
        ];
        assert_eq!(answered_question(&details), Some((1, "Which?".into(), "Answer: B".into())));
        let mut d2 = details.clone();
        d2[1]["value"]["surfaced"] = serde_json::json!(true);
        assert!(answered_question(&d2).is_none(), "a surfaced answer is history");
    }
}
