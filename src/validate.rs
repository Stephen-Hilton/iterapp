use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::markers::{self, Role};

/// `iter validate` — deterministic checks (and light corrections) for `*.iter.md`
/// files, role-aware per the filename rule (markers::role_of).
///
/// Split by what a machine can safely do:
/// - FIXABLE (applied with --fix): purely mechanical normalizations whose result
///   is provably what the author meant — a `...` closing fence, junk before the
///   opening fence, an unquoted prose value containing ": " (the measured PyYAML
///   footgun from pdy work item 71123487).
/// - REPORT-ONLY: anything that needs a human/agent decision — missing keys,
///   missing files, filenames matching no role, stray identity keys.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Would break or mislead the engine — must be dealt with.
    Error,
    /// Works, but confusing or incomplete — should be dealt with.
    Warn,
    /// Worth knowing; often deliberate (e.g. no `testgroup:` key).
    Info,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub file: String,
    pub severity: Severity,
    /// Short machine-stable code, e.g. "unquoted-prose".
    pub code: &'static str,
    /// One-sentence human description of the exact problem.
    pub message: String,
    /// True when --fix repaired it in this run.
    pub fixed: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub files_checked: usize,
    pub findings: Vec<Finding>,
    pub fixed: usize,
}

impl Report {
    pub fn worst(&self) -> Option<Severity> {
        if self.findings.iter().any(|f| !f.fixed && f.severity == Severity::Error) {
            Some(Severity::Error)
        } else if self.findings.iter().any(|f| !f.fixed && f.severity == Severity::Warn) {
            Some(Severity::Warn)
        } else if self.findings.iter().any(|f| !f.fixed) {
            Some(Severity::Info)
        } else {
            None
        }
    }
}

/// Prose frontmatter keys that must be quoted when they contain a colon-space —
/// unquoted, strict-YAML readers see a nested key and refuse the whole block.
const PROSE_KEYS: &[&str] = &["name", "description", "endpoint"];

/// Identity-bearing keys that belong to specific roles; anywhere else they are
/// harmless (the filename wins) but confuse readers.
fn stray_identity_keys(role: Option<Role>) -> &'static [&'static str] {
    match role {
        Some(Role::Marker) => &["interface", "participants"],
        Some(Role::Interface) => &["level", "participants"],
        Some(Role::Usecase) => &["level", "interface"],
        _ => &["level", "interface", "participants"],
    }
}

/// Validate one file. When `fix` is true the safe corrections are written back
/// (atomically) and reported with `fixed: true`.
pub fn validate_file(path: &Path, fix: bool) -> std::io::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let fname = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let file_s = path.to_string_lossy().into_owned();
    let original = std::fs::read_to_string(path)?;
    let mut text = original.clone();
    let role = markers::role_of(&fname);

    let mut push = |severity: Severity, code: &'static str, message: String, fixed: bool| {
        findings.push(Finding { file: file_s.clone(), severity, code, message, fixed });
    };

    // 1. Filename role.
    if role.is_none() {
        push(
            Severity::Warn,
            "no-role",
            "filename matches no role (marker/bizreq/techreq/interface/testgroup/usecase before .iter.md) — the engine treats it as a plain context doc; rename it if it was meant to be more".into(),
            false,
        );
    }

    // 2. Mechanical normalizations (fixable).
    // 2a. Junk before the opening fence (BOM, blank lines, spaces).
    let trimmed_start = text.trim_start_matches(['\u{feff}', ' ', '\t', '\n', '\r']);
    if trimmed_start.starts_with("---") && trimmed_start.len() != text.len() {
        push(
            Severity::Warn,
            "junk-before-fence",
            "whitespace or a byte-order mark sits before the opening `---` fence, hiding the frontmatter from strict parsers".into(),
            fix,
        );
        if fix {
            text = trimmed_start.to_string();
        }
    }
    // 2b. `...` used as the closing fence (YAML allows it; the engine does not).
    if let Some(rest) = text.strip_prefix("---\n") {
        let has_dash_close = rest.contains("\n---");
        let dots = rest.find("\n...");
        if !has_dash_close {
            if let Some(dot_pos) = dots {
                push(
                    Severity::Warn,
                    "dots-fence",
                    "frontmatter closes with `...` — the engine only recognizes `---`".into(),
                    fix,
                );
                if fix {
                    let abs = 4 + dot_pos; // offset of the "\n..." within `text`
                    text.replace_range(abs..abs + 4, "\n---");
                }
            } else {
                push(
                    Severity::Error,
                    "unterminated-frontmatter",
                    "the opening `---` fence never closes — the whole file reads as frontmatter and the engine sees no body and no keys".into(),
                    false,
                );
            }
        }
    }

    // Re-parse after mechanical fixes.
    let (front, participants, body) = markers::frontmatter(&text);
    let has_frontmatter = text.trim_start().starts_with("---") && !front.is_empty();

    // 2c. Unquoted prose values containing ": " (fixable) — measured to make
    // PyYAML refuse 18 of 37 blocks (pdy work item 71123487). Only frontmatter
    // lines are touched: the region between the opening fence and the first
    // closing fence — body prose that happens to start with `name:` is left alone.
    if has_frontmatter {
        let close_line = text
            .lines()
            .enumerate()
            .skip(1)
            .find(|(_, l)| l.trim() == "---")
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut fixed_lines = Vec::new();
        let rebuilt: Vec<String> = text
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 || i >= close_line {
                    return line.to_string();
                }
                let t = line.trim_start();
                for key in PROSE_KEYS {
                    if let Some(raw) = t.strip_prefix(&format!("{}:", key)) {
                        let val = raw.trim();
                        let quoted = (val.starts_with('"') && val.ends_with('"'))
                            || (val.starts_with('\'') && val.ends_with('\''));
                        if !quoted && val.contains(": ") && !val.contains('"') {
                            fixed_lines.push(format!("{}: {}", key, val));
                            if fix {
                                let indent = &line[..line.len() - t.len()];
                                return format!("{}{}: \"{}\"", indent, key, val);
                            }
                        }
                    }
                }
                line.to_string()
            })
            .collect();
        for l in &fixed_lines {
            push(
                Severity::Error,
                "unquoted-prose",
                format!("`{}` contains a colon-space and is unquoted — strict YAML readers refuse the whole block; quote the value", l),
                fix,
            );
        }
        if fix && !fixed_lines.is_empty() {
            let mut joined = rebuilt.join("\n");
            if text.ends_with('\n') {
                joined.push('\n');
            }
            text = joined;
        }
    }

    // 3. Role-specific expectations (report-only).
    match role {
        Some(Role::Marker) => {
            if !has_frontmatter {
                push(
                    Severity::Error,
                    "missing-frontmatter",
                    "a marker file needs `---`-fenced frontmatter (name, level, description) — without it the node has no name or level on the Projects map".into(),
                    false,
                );
            } else {
                for (key, sev) in [("name", Severity::Warn), ("level", Severity::Error), ("description", Severity::Warn)] {
                    if front.get(key).map(|v| v.trim().is_empty()).unwrap_or(true) {
                        push(sev, "missing-key", format!("marker frontmatter has no `{}:` — the Projects map shows a fallback or nothing", key), false);
                    }
                }
                if let Some(level) = front.get("level") {
                    if !["project", "context", "container", "component"].contains(&level.as_str()) {
                        push(Severity::Error, "bad-level", format!("`level: {}` is not one of project|context|container|component", level), false);
                    }
                }
                match front.get("testgroup") {
                    None => push(
                        Severity::Info,
                        "no-testgroup-key",
                        "no `testgroup:` key — this C4 object is deliberately outside the test sweep (declare the key if tests should exist)".into(),
                        false,
                    ),
                    Some(tg) => {
                        let tg_path = path.parent().unwrap_or(Path::new(".")).join(tg.trim());
                        if !tg_path.is_file() {
                            push(Severity::Error, "declared-testgroup-missing", format!("`testgroup: {}` is declared but the file does not exist — a testwriter item should create it", tg), false);
                        }
                        if front.get("test_dir").map(|v| v.trim().is_empty()).unwrap_or(true) {
                            push(Severity::Warn, "missing-key", "`testgroup:` is declared but `test_dir:` is not — the lock carve for parallel code/testwriter items falls back to a guess".into(), false);
                        }
                    }
                }
                if !body.to_lowercase().contains("long description") {
                    push(Severity::Info, "no-long-description", "marker body has no `# Long Description` section (`iter stubdesc` can stub it; agents should write it for real)".into(), false);
                }
            }
        }
        Some(Role::Interface) => {
            if front.get("interface").map(|v| v.trim().is_empty()).unwrap_or(true) {
                push(Severity::Error, "missing-key", "interface file has no `interface:` id — nothing can reference it from uses:/provides:".into(), false);
            }
            if front.get("kind").map(|v| v.trim().is_empty()).unwrap_or(true) {
                push(Severity::Warn, "missing-key", "interface file has no `kind:` (http | grpc | kafka | sql | file | cli | library | …)".into(), false);
            }
        }
        Some(Role::Usecase) => {
            if front.get("name").map(|v| v.trim().is_empty()).unwrap_or(true) {
                push(Severity::Warn, "missing-key", "usecase file has no `name:` — it lists blank in the Projects view".into(), false);
            }
            if participants.is_empty() {
                push(Severity::Info, "no-participants", "no `participants:` lines — the use-case exists but threads nothing through the tree".into(), false);
            }
        }
        Some(Role::Testgroup) => {
            if !text.contains(crate::testgroups::BLOCK_START) {
                push(Severity::Warn, "no-testgroups-block", "no `<!-- iterapp:testgroups -->` block — no groups are defined here yet".into(), false);
            } else {
                for group in crate::testgroups::parse(&text) {
                    for t in &group.testlist {
                        let script = path.parent().unwrap_or(Path::new(".")).join(&t.shell);
                        if !script.is_file() {
                            push(
                                Severity::Error,
                                "missing-test-script",
                                format!("group \"{}\" registers `{}` but the script does not exist", group.label, t.shell),
                                false,
                            );
                        }
                    }
                }
            }
        }
        _ => {}
    }

    // 4. Stray identity keys: harmless (the filename wins) but confusing.
    if has_frontmatter {
        for key in stray_identity_keys(role) {
            let present = if *key == "participants" { !participants.is_empty() } else { front.contains_key(*key) };
            if present {
                push(
                    Severity::Warn,
                    "stray-identity-key",
                    format!("`{}:` has no meaning in a {} file — the filename decides the role; remove the key or rename the file", key, role_name(role)),
                    false,
                );
            }
        }
    }

    if fix && text != original {
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(findings)
}

fn role_name(role: Option<Role>) -> &'static str {
    match role {
        Some(Role::Marker) => "marker",
        Some(Role::Bizreq) => "bizreq",
        Some(Role::Techreq) => "techreq",
        Some(Role::Interface) => "interface",
        Some(Role::Testgroup) => "testgroup",
        Some(Role::Usecase) => "usecase",
        None => "plain (no-role)",
    }
}

/// Validate one file or every `*.iter.md` under the given roots.
pub fn run(roots: &[PathBuf], single: Option<&Path>, fix: bool) -> std::io::Result<Report> {
    let mut report = Report::default();
    let files: Vec<PathBuf> = match single {
        Some(f) => vec![f.to_path_buf()],
        None => {
            let mut out = Vec::new();
            for root in roots {
                collect_iter_files(root, &mut out);
            }
            out.sort();
            out.dedup();
            out
        }
    };
    for file in files {
        report.files_checked += 1;
        match validate_file(&file, fix) {
            Ok(fs) => {
                report.fixed += fs.iter().filter(|f| f.fixed).count();
                report.findings.extend(fs);
            }
            Err(e) => report.findings.push(Finding {
                file: file.to_string_lossy().into_owned(),
                severity: Severity::Error,
                code: "unreadable",
                message: format!("cannot read: {}", e),
                fixed: false,
            }),
        }
    }
    Ok(report)
}

fn collect_iter_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(name.as_str(), ".git" | "target" | "node_modules") {
                continue;
            }
            collect_iter_files(&path, out);
        } else if name.ends_with(".iter.md") {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-validate-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn codes(fs: &[Finding]) -> Vec<&'static str> {
        fs.iter().map(|f| f.code).collect()
    }

    #[test]
    fn clean_marker_passes_with_info_only() {
        let dir = tmp("clean");
        std::fs::create_dir_all(dir.join("test")).unwrap();
        std::fs::write(dir.join("test/testgroup.iter.md"), "# tests\n").unwrap();
        let p = dir.join("comp.marker.iter.md");
        std::fs::write(&p, "---\nname: \"Comp\"\nlevel: component\ndescription: \"a thing\"\ntestgroup: test/testgroup.iter.md\ntest_dir: test\n---\n# Long Description\nreal words\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(fs.iter().all(|f| f.severity == Severity::Info), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unquoted_prose_is_flagged_and_fixed() {
        let dir = tmp("quote");
        let p = dir.join("comp.marker.iter.md");
        std::fs::write(&p, "---\nname: Comp\nlevel: component\ndescription: svc-intake: thin host for m08\n---\nbody\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"unquoted-prose"), "{:?}", fs);
        assert!(!fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);

        let fs = validate_file(&p, true).unwrap();
        assert!(fs.iter().find(|f| f.code == "unquoted-prose").unwrap().fixed);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("description: \"svc-intake: thin host for m08\""), "{}", text);
        // Idempotent: a second pass finds nothing to quote.
        let fs = validate_file(&p, true).unwrap();
        assert!(!codes(&fs).contains(&"unquoted-prose"), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dots_fence_and_leading_junk_are_fixed() {
        let dir = tmp("fence");
        let p = dir.join("x.marker.iter.md");
        std::fs::write(&p, "\n\n---\nname: \"X\"\nlevel: component\ndescription: \"d\"\n...\nbody\n").unwrap();
        let fs = validate_file(&p, true).unwrap();
        let c = codes(&fs);
        assert!(c.contains(&"junk-before-fence") && c.contains(&"dots-fence"), "{:?}", fs);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("---\n") && text.contains("\n---\nbody"), "{}", text);
        // The engine can now parse it.
        let (front, _, _) = markers::frontmatter(&text);
        assert_eq!(front.get("name").map(String::as_str), Some("X"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unterminated_frontmatter_is_an_error_not_a_guessed_fix() {
        let dir = tmp("unterminated");
        let p = dir.join("x.marker.iter.md");
        let body = "---\nname: \"X\"\nlevel: component\nno closing fence anywhere\n";
        std::fs::write(&p, body).unwrap();
        let fs = validate_file(&p, true).unwrap();
        assert!(codes(&fs).contains(&"unterminated-frontmatter"), "{:?}", fs);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), body, "no guessed rewrite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn role_specific_reports() {
        let dir = tmp("roles");
        // Marker: missing level + declared-but-missing testgroup file.
        let m = dir.join("m.marker.iter.md");
        std::fs::write(&m, "---\nname: \"M\"\ndescription: \"d\"\ntestgroup: test/testgroup.iter.md\n---\nbody").unwrap();
        let fs = validate_file(&m, false).unwrap();
        let c = codes(&fs);
        assert!(c.contains(&"missing-key") && c.contains(&"declared-testgroup-missing"), "{:?}", fs);

        // Interface without an id.
        let i = dir.join("api.interface.iter.md");
        std::fs::write(&i, "---\nkind: http\n---\ncontract").unwrap();
        assert!(codes(&validate_file(&i, false).unwrap()).contains(&"missing-key"));

        // Stray level: in a usecase file — filename wins, key flagged.
        let u = dir.join("flow.usecase.iter.md");
        std::fs::write(&u, "---\nname: \"F\"\nlevel: container\nparticipants:\n  - 1 core\n---\nstory").unwrap();
        assert!(codes(&validate_file(&u, false).unwrap()).contains(&"stray-identity-key"));

        // No-role filename.
        let n = dir.join("notes.iter.md");
        std::fs::write(&n, "just notes\n").unwrap();
        assert!(codes(&validate_file(&n, false).unwrap()).contains(&"no-role"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testgroup_missing_script_is_an_error() {
        let dir = tmp("tgscripts");
        let p = dir.join("testgroup.iter.md");
        std::fs::write(&p, "# t\n<!-- iterapp:testgroups\n{\"label\":\"G\",\"testlist\":[{\"id\":\"t1\",\"name\":\"t\",\"desc\":\"\",\"shell\":\"tests/iter/t1.sh\"}]}\n-->\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(codes(&fs).contains(&"missing-test-script"), "{:?}", fs);
        std::fs::create_dir_all(dir.join("tests/iter")).unwrap();
        std::fs::write(dir.join("tests/iter/t1.sh"), "exit 0\n").unwrap();
        let fs = validate_file(&p, false).unwrap();
        assert!(!codes(&fs).contains(&"missing-test-script"), "{:?}", fs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_walks_roots_and_counts() {
        let dir = tmp("walk");
        std::fs::write(dir.join("a.marker.iter.md"), "---\nname: \"A\"\nlevel: component\ndescription: \"d\"\n---\n# Long Description\nx\n").unwrap();
        std::fs::write(dir.join("notes.iter.md"), "plain\n").unwrap();
        let report = run(&[dir.clone()], None, false).unwrap();
        assert_eq!(report.files_checked, 2);
        assert_eq!(report.worst(), Some(Severity::Warn), "{:?}", report.findings); // no-role warn
        let _ = std::fs::remove_dir_all(&dir);
    }
}
