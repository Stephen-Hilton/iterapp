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
    // Capability files (features item 12): mechanics an agent needs only
    // occasionally, indexed from _shared.md and read on demand. They must ship
    // with the template — an index line pointing at a file `iter init` never
    // created is a capability the agent cannot use.
    (".iter/agents/_capability/_create_new_workitem.md", include_str!(".iter/agents/_capability/_create_new_workitem.md")),
    (".iter/agents/_capability/_ask_the_human.md", include_str!(".iter/agents/_capability/_ask_the_human.md")),
    (".iter/agents/_capability/_critical_review.md", include_str!(".iter/agents/_capability/_critical_review.md")),
    (".iter/agents/_capability/_reject_invalid_work.md", include_str!(".iter/agents/_capability/_reject_invalid_work.md")),
    (".iter/agents/_capability/_runtests.md", include_str!(".iter/agents/_capability/_runtests.md")),
    (".iter/agents/_capability/_testgroup_authoring.md", include_str!(".iter/agents/_capability/_testgroup_authoring.md")),
    (".iter/agents/_capability/_iter_file_authoring.md", include_str!(".iter/agents/_capability/_iter_file_authoring.md")),
    (".iter/agents/_capability/_interface_contracts.md", include_str!(".iter/agents/_capability/_interface_contracts.md")),
    (".iter/agents/_capability/_teststate.md", include_str!(".iter/agents/_capability/_teststate.md")),
    (".iter/agents/_capability/_usecase_links.md", include_str!(".iter/agents/_capability/_usecase_links.md")),
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

/// Create any template file missing under `root`. Returns how many were added.
/// The two structureV2 head files (.iter/config.iter.json + main.iter.md) heal
/// separately via project::ensure_head_files, since the mainfile's location is
/// itself configured.
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

    /// features item 12: `_shared.md` carries an INDEX of capability files that
    /// agents read on demand. An index line naming a file the template never
    /// ships is a capability the agent cannot reach — it reads as deleted. Both
    /// directions are checked, so neither a renamed file nor an orphaned one
    /// can pass unnoticed.
    #[test]
    fn every_indexed_capability_ships_and_every_shipped_one_is_indexed() {
        let shared = TEMPLATE
            .iter()
            .find(|(rel, _)| *rel == ".iter/agents/_shared.md")
            .map(|(_, body)| *body)
            .expect("_shared.md is in the template");
        let shipped: Vec<&str> = TEMPLATE
            .iter()
            .filter_map(|(rel, _)| rel.strip_prefix(".iter/agents/_capability/"))
            .collect();
        assert!(!shipped.is_empty(), "the capability split shipped no files");

        for name in &shipped {
            assert!(
                shared.contains(name),
                "{} ships but nothing in _shared.md's index points at it — no agent will ever read it",
                name
            );
        }
        // And the reverse: every *.md the index names must be a shipped file.
        for line in shared.lines() {
            for token in line.split('`') {
                // `<file>` and the like are prose placeholders, not references.
                let token = token.trim_start_matches("_capability/");
                if token.starts_with('_')
                    && token.ends_with(".md")
                    && token != "_shared.md"
                    && !token.contains('<')
                {
                    assert!(
                        shipped.contains(&token),
                        "_shared.md indexes {} but the template does not ship it — `iter init` \
                         would scaffold a project whose index points at a missing file",
                        token
                    );
                }
            }
        }
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
