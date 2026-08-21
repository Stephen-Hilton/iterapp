use std::path::Path;

/// The `.iter/` template, embedded in the binary at compile time so a deployed
/// `iterloop` is one self-contained file: copy it into a project directory, start
/// it, and any missing `.iter/` folders/files are created on the spot. Existing
/// files are NEVER overwritten — healing adds, it doesn't reset.
pub const TEMPLATE: &[(&str, &str)] = &[
    (".iter/agents/_shared.md", include_str!(".iter/agents/_shared.md")),
    (".iter/agents/_critic.md", include_str!(".iter/agents/_critic.md")),
    (".iter/agents/code.md", include_str!(".iter/agents/code.md")),
    (".iter/agents/ingest.md", include_str!(".iter/agents/ingest.md")),
    (".iter/agents/plan.md", include_str!(".iter/agents/plan.md")),
    (".iter/agents/refactor.md", include_str!(".iter/agents/refactor.md")),
    (".iter/agents/testwriter.md", include_str!(".iter/agents/testwriter.md")),
    (".iter/agents/usecase.md", include_str!(".iter/agents/usecase.md")),
    (".iter/prepostwork/deploy.md", include_str!(".iter/prepostwork/deploy.md")),
    (".iter/prepostwork/git-commit.md", include_str!(".iter/prepostwork/git-commit.md")),
    (".iter/prepostwork/git-pr.md", include_str!(".iter/prepostwork/git-pr.md")),
    (".iter/prepostwork/git-pull.md", include_str!(".iter/prepostwork/git-pull.md")),
    (".iter/prepostwork/git-push.md", include_str!(".iter/prepostwork/git-push.md")),
    (".iter/prepostwork/iterloop-stop.md", include_str!(".iter/prepostwork/iterloop-stop.md")),
    (".iter/prepostwork/iterloop-wait-for-stop.md", include_str!(".iter/prepostwork/iterloop-wait-for-stop.md")),
    (".iter/source/agent.md", include_str!(".iter/source/agent.md")),
    (".iter/source/error.md", include_str!(".iter/source/error.md")),
    (".iter/source/user.md", include_str!(".iter/source/user.md")),
    (".iter/.engine/config.json", include_str!(".iter/.engine/config.json")),
    (".iter/.engine/statusline-collector.py", include_str!(".iter/.engine/statusline-collector.py")),
    (".iter/.engine/codepath_lock.md", include_str!(".iter/.engine/codepath_lock.md")),
    (".iter/.engine/codepath_unlock.md", include_str!(".iter/.engine/codepath_unlock.md")),
    (".iter/.engine/workitems.jsonl", include_str!(".iter/.engine/workitems.jsonl")),
    (".iter/.engine/workitems_closed.jsonl", include_str!(".iter/.engine/workitems_closed.jsonl")),
];

/// The GLOBAL requirement stubs live outside the static template: they heal at
/// their CONFIGURED paths (globalsettings.global_bizreq_path / global_techreq_path,
/// default `{codepath}/reqs/`), not at a fixed `.iter/` slot — so a project that
/// keeps its real requirements elsewhere never grows a shadowing stub copy.
/// Never overwrites; returns how many were created.
pub fn ensure_global_reqs(project_root: &Path, cfg: &crate::config::Config) -> std::io::Result<usize> {
    let stubs = [
        (crate::config::global_bizreq(project_root, cfg), include_str!(".iter/reqs/bizreq.iter.md")),
        (crate::config::global_techreq(project_root, cfg), include_str!(".iter/reqs/techreq.iter.md")),
    ];
    let mut created = 0;
    for (path, content) in stubs {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)?;
            created += 1;
        }
    }
    Ok(created)
}

/// Create any template file missing under `root`. Returns how many were added.
pub fn ensure_project(root: &Path) -> std::io::Result<usize> {
    let mut created = 0;
    for (rel, content) in TEMPLATE {
        let path = root.join(rel);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)?;
            created += 1;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iterloop-tpl-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scaffolds_full_tree_from_nothing() {
        let root = tmpdir("scaffold");
        let n = ensure_project(&root).unwrap();
        assert_eq!(n, TEMPLATE.len());
        assert!(root.join(".iter/agents/code.md").is_file());
        assert!(root.join(".iter/.engine/config.json").is_file());
        // Embedded content is the real template, not stubs.
        let code = std::fs::read_to_string(root.join(".iter/agents/code.md")).unwrap();
        assert!(code.contains("max_agent_count"));
        // Second pass adds nothing.
        assert_eq!(ensure_project(&root).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn global_reqs_heal_at_configured_paths() {
        let root = tmpdir("greqs");
        let mut cfg = crate::config::Config::default();
        cfg.globalsettings.global_bizreq_path = "{codepath}/reqs/bizreq.iter.md".into();
        cfg.globalsettings.global_techreq_path = "docs/techreq.iter.md".into();
        assert_eq!(ensure_global_reqs(&root, &cfg).unwrap(), 2);
        assert!(root.join("reqs/bizreq.iter.md").is_file());
        assert!(root.join("docs/techreq.iter.md").is_file());
        // Second pass adds nothing; real content is never overwritten.
        std::fs::write(root.join("reqs/bizreq.iter.md"), "BR-1: the real thing").unwrap();
        assert_eq!(ensure_global_reqs(&root, &cfg).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(root.join("reqs/bizreq.iter.md")).unwrap(), "BR-1: the real thing");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn heals_missing_without_touching_existing() {
        let root = tmpdir("heal");
        ensure_project(&root).unwrap();
        std::fs::write(root.join(".iter/agents/code.md"), "customized by user").unwrap();
        std::fs::remove_file(root.join(".iter/prepostwork/deploy.md")).unwrap();
        let n = ensure_project(&root).unwrap();
        assert_eq!(n, 1, "only the deleted file comes back");
        assert_eq!(
            std::fs::read_to_string(root.join(".iter/agents/code.md")).unwrap(),
            "customized by user",
            "existing files are never overwritten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
