use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BLOCK_START: &str = "<!-- iterapp:testgroups";
pub const BLOCK_END: &str = "-->";

/// One registered test: a shell script plus its human-facing identity. The script
/// is the entire contract (see features/TDD.md "Test Contract"): exit 0 = green,
/// 1 = red, anything else = the script itself broke (`error`); the last stdout line
/// may be `ITER_RESULT pass=X fail=Y total=Z` for per-test counts.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, from = "TestEntryDe")]
pub struct TestEntry {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub shell: String,
}

/// Back-compat: testlists written before the structured schema were bare script
/// names (`"testscript03.sh"`). Those deserialize into a full entry with the id
/// derived from the filename stem.
#[derive(Deserialize)]
#[serde(untagged)]
enum TestEntryDe {
    Script(String),
    Entry {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        desc: String,
        #[serde(default)]
        shell: String,
    },
}

impl From<TestEntryDe> for TestEntry {
    fn from(de: TestEntryDe) -> TestEntry {
        match de {
            TestEntryDe::Script(shell) => {
                let id = shell.trim_end_matches(".sh").to_string();
                TestEntry { id: id.clone(), name: id, desc: String::new(), shell }
            }
            TestEntryDe::Entry { id, name, desc, shell } => {
                let id = if id.is_empty() { shell.trim_end_matches(".sh").to_string() } else { id };
                TestEntry { id, name, desc, shell }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TestGroup {
    pub label: String,
    /// What this group is supposed to prove — surfaced in the testing UI.
    pub desc: String,
    /// Gates the STATE of sweep-born fix items, never their existence: red run →
    /// fix item `queued` when true (work proceeds next pick), `todo` when false
    /// (sits for human review). Defaults false.
    pub auto_fix: bool,
    pub lastrun: String,
    pub result: String,
    pub counts: String,
    pub testlist: Vec<TestEntry>,
}

impl TestGroup {
    /// "Provably green right now": the last recorded run passed.
    pub fn is_green(&self) -> bool {
        self.result == "passed"
    }
}

/// Parse the `iterapp:testgroups` JSONL block from a testgroups.iter.md document.
/// A missing block means "never tested": returns an empty list.
pub fn parse(content: &str) -> Vec<TestGroup> {
    let Some(start) = content.find(BLOCK_START) else { return Vec::new() };
    let after = &content[start + BLOCK_START.len()..];
    let Some(end) = after.find(BLOCK_END) else { return Vec::new() };
    let mut groups = Vec::new();
    for line in after[..end].lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TestGroup>(line) {
            Ok(g) => groups.push(g),
            Err(e) => eprintln!("warning: bad testgroups line ({}): {}", e, line),
        }
    }
    groups
}

/// Replace (or append) the `iterapp:testgroups` block with the given groups,
/// leaving all human-facing markdown untouched.
pub fn update(content: &str, groups: &[TestGroup]) -> String {
    let mut block = String::from(BLOCK_START);
    block.push('\n');
    for g in groups {
        block.push_str(&serde_json::to_string(g).expect("testgroup serializes"));
        block.push('\n');
    }
    block.push_str(BLOCK_END);

    if let Some(start) = content.find(BLOCK_START) {
        if let Some(end_rel) = content[start..].find(BLOCK_END) {
            let end = start + end_rel + BLOCK_END.len();
            return format!("{}{}{}", &content[..start], block, &content[end..]);
        }
    }
    format!("{}\n\n{}\n", content.trim_end(), block)
}

/// Every testgroup file under `code_root` (skipping VCS/build noise), identified
/// by FILENAME role: any `*testgroup.iter.md` (see markers::role_of). Scripts and
/// the `runs/` history resolve relative to the file's directory; which C4 object
/// OWNS a file is declared by that object's marker (`testgroup:` key), never
/// inferred from position.
pub fn find_files(code_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(code_root, &mut out);
    out.sort();
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "node_modules" | ".iter") {
                continue;
            }
            collect_files(&path, out);
        } else if crate::markers::role_of(&name) == Some(crate::markers::Role::Testgroup) {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two groups in one file — the shape that exposed the sweep's same-file
    /// write race (V2 read this from a since-deleted sample project).
    #[test]
    fn parses_two_groups_in_one_file() {
        let doc = "# parser tests\n\n<!-- iterapp:testgroups\n\
            {\"label\":\"parser decisions\",\"result\":\"passed\",\"counts\":\"3/3\",\"testlist\":[{\"id\":\"t1\",\"name\":\"decisions\",\"desc\":\"\",\"shell\":\"t1-decisions.sh\"}]}\n\
            {\"label\":\"parser refusals\",\"testlist\":[{\"id\":\"t2\",\"name\":\"refusals\",\"desc\":\"\",\"shell\":\"t2-refusals.sh\"}]}\n\
            -->\n";
        let groups = parse(doc);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "parser decisions");
        assert_eq!(groups[0].testlist[0].shell, "t1-decisions.sh");
        assert_eq!(groups[1].label, "parser refusals");
        assert_eq!(groups[1].testlist[0].shell, "t2-refusals.sh");
        assert!(groups[0].is_green());
        assert!(!groups[1].is_green(), "never run = not provably green");
    }

    #[test]
    fn bare_string_testlist_still_parses() {
        let doc = "<!-- iterapp:testgroups\n{\"label\":\"legacy\",\"testlist\":[\"testscript03.sh\",{\"id\":\"t2\",\"name\":\"named\",\"desc\":\"d\",\"shell\":\"t2.sh\"}]}\n-->";
        let groups = parse(doc);
        assert_eq!(groups[0].testlist.len(), 2);
        assert_eq!(groups[0].testlist[0].id, "testscript03");
        assert_eq!(groups[0].testlist[0].shell, "testscript03.sh");
        assert_eq!(groups[0].testlist[1].id, "t2");
        assert_eq!(groups[0].testlist[1].name, "named");
        assert!(!groups[0].auto_fix, "auto_fix defaults false");
    }

    #[test]
    fn update_roundtrip_preserves_prose() {
        let doc = "# My tests\n\nprose stays\n\n<!-- iterapp:testgroups\n{\"label\":\"a\",\"lastrun\":\"\",\"result\":\"\",\"counts\":\"\",\"testlist\":[]}\n-->\n";
        let mut groups = parse(doc);
        groups[0].result = "passed".into();
        groups[0].counts = "5/5".into();
        let updated = update(doc, &groups);
        assert!(updated.contains("prose stays"));
        let reparsed = parse(&updated);
        assert_eq!(reparsed[0].result, "passed");
        assert_eq!(reparsed[0].counts, "5/5");
        assert!(reparsed[0].is_green());
    }

    #[test]
    fn update_appends_block_when_missing() {
        let doc = "# no block here\n";
        let groups = vec![TestGroup { label: "g".into(), ..Default::default() }];
        let updated = update(doc, &groups);
        assert_eq!(parse(&updated).len(), 1);
        assert!(updated.starts_with("# no block here"));
    }

    #[test]
    fn finds_testgroup_files_by_filename_role() {
        let root = std::env::temp_dir().join(format!("iter-tgfind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("comp/test")).unwrap();
        std::fs::create_dir_all(root.join("target/skip")).unwrap();
        std::fs::write(root.join("comp/test/testgroup.iter.md"), "x").unwrap();
        std::fs::write(root.join("comp/extra.testgroup.iter.md"), "x").unwrap();
        std::fs::write(root.join("comp/testgroups.iter.md"), "x").unwrap(); // old plural: NOT the role
        std::fs::write(root.join("target/skip/testgroup.iter.md"), "x").unwrap();
        let found = find_files(&root);
        assert_eq!(found.len(), 2, "singular-suffix files only, target/ skipped: {:?}", found);
        let _ = std::fs::remove_dir_all(&root);
    }
}
