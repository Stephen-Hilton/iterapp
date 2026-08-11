use serde::{Deserialize, Serialize};

pub const BLOCK_START: &str = "<!-- iterapp:testgroups";
pub const BLOCK_END: &str = "-->";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TestGroup {
    pub label: String,
    pub lastrun: String,
    pub result: String,
    pub counts: String,
    pub testlist: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_testgroups() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample/test/testgroups.iter.md");
        let content = std::fs::read_to_string(path).unwrap();
        let groups = parse(&content);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].label, "default greeting");
        assert_eq!(groups[2].testlist, vec!["testscript03.sh"]);
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
    }

    #[test]
    fn update_appends_block_when_missing() {
        let doc = "# no block here\n";
        let groups = vec![TestGroup { label: "g".into(), ..Default::default() }];
        let updated = update(doc, &groups);
        assert_eq!(parse(&updated).len(), 1);
        assert!(updated.starts_with("# no block here"));
    }
}
