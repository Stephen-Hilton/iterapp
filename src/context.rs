use std::path::{Path, PathBuf};

/// The project-wide requirements directory: the first-class home of the global
/// `bizreq.iter.md` / `techreq.iter.md` (component-local req files stay beside their
/// component). Default `<engine home>/.iter/reqs/`; a project relocates it with a
/// `reqs:` frontmatter key on the project-level marker at the code root — relative
/// values resolve against the code root, `~` works.
pub fn reqs_dir(engine_home: &Path, code_root: &Path) -> PathBuf {
    let mut candidates: Vec<(bool, String)> = Vec::new(); // (is level:project, reqs value)
    let mut names: Vec<PathBuf> = std::fs::read_dir(code_root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    // Only marker files can carry the reqs: override — the filename
                    // declares the role (markers::role_of).
                    p.is_file() && crate::markers::role_of(&name) == Some(crate::markers::Role::Marker)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for path in names {
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let (front, _, _) = crate::markers::frontmatter(&content);
        if let (Some(level), Some(reqs)) = (front.get("level"), front.get("reqs")) {
            if !reqs.trim().is_empty() {
                candidates.push((level == "project", reqs.trim().to_string()));
            }
        }
    }
    let chosen = candidates
        .iter()
        .find(|(is_project, _)| *is_project)
        .or_else(|| candidates.first())
        .map(|(_, v)| v.clone());
    match chosen {
        Some(mut raw) => {
            if let Some(rest) = raw.strip_prefix("~/") {
                if let Some(home) = std::env::var_os("HOME") {
                    raw = format!("{}/{}", home.to_string_lossy(), rest);
                }
            }
            let p = PathBuf::from(&raw);
            if p.is_absolute() { p } else { code_root.join(p) }
        }
        None => engine_home.join(".iter").join("reqs"),
    }
}

/// The markdown files sitting in the reqs dir, sorted — what the engine surfaces to
/// every agent as project-wide requirements. Non-recursive by design: the dir IS the
/// concept; anything deeper is the project's own organization to reference via globs.
pub fn reqs_files(reqs: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(reqs)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().map(|e| e == "md").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Resolve a work item's context patterns into concrete files, deterministically.
/// Substitutes `{codepath}`, `{reqs}`, and `~`, resolves relative patterns against
/// the project root, expands globs. Returns (files, warnings) — a pattern that
/// matches nothing is a warning, never an error.
pub fn resolve(patterns: &[String], codepath: &Path, project_root: &Path, reqs: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for pattern in patterns {
        let mut p = pattern
            .replace("{codepath}", &codepath.to_string_lossy())
            .replace("{reqs}", &reqs.to_string_lossy());
        if let Some(rest) = p.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                p = format!("{}/{}", home.to_string_lossy(), rest);
            }
        }
        let abs = if Path::new(&p).is_absolute() {
            p.clone()
        } else {
            project_root.join(&p).to_string_lossy().into_owned()
        };
        if abs.contains('*') || abs.contains('?') || abs.contains('[') {
            match glob::glob(&abs) {
                Ok(paths) => {
                    let mut hit = false;
                    for entry in paths.flatten() {
                        if entry.is_file() {
                            files.push(entry);
                            hit = true;
                        }
                    }
                    if !hit {
                        warnings.push(format!("context pattern matched nothing: {}", pattern));
                    }
                }
                Err(e) => warnings.push(format!("bad context pattern {}: {}", pattern, e)),
            }
        } else {
            let path = PathBuf::from(&abs);
            if path.is_file() {
                files.push(path);
            } else {
                warnings.push(format!("context file not found: {}", pattern));
            }
        }
    }
    files.sort();
    files.dedup();
    (files, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_sample_context() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let reqs = reqs_dir(&root, &root);
        let patterns = vec!["./bizreq.md".to_string(), "./*req.md".to_string(), "./missing.md".to_string()];
        let (files, warnings) = resolve(&patterns, &root, &root, &reqs);
        assert_eq!(files.len(), 2, "bizreq + techreq, deduped: {:?}", files);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.md"));
    }

    #[test]
    fn codepath_substitution() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let codepath = root.join("test");
        let patterns = vec!["{codepath}/testgroup.iter.md".to_string()];
        let (files, warnings) = resolve(&patterns, &codepath, &root, &root.join(".iter/reqs"));
        assert_eq!(files.len(), 1);
        assert!(warnings.is_empty());
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-reqs-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reqs_dir_defaults_to_engine_iter_reqs() {
        let dir = tmpdir("default");
        assert_eq!(reqs_dir(&dir, &dir), dir.join(".iter/reqs"));
        // A root marker WITHOUT a reqs: key changes nothing.
        std::fs::write(dir.join("proj.marker.iter.md"), "---\nname: P\nlevel: project\n---\nbody").unwrap();
        assert_eq!(reqs_dir(&dir, &dir), dir.join(".iter/reqs"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reqs_dir_honors_project_marker_override() {
        let dir = tmpdir("override");
        std::fs::write(
            dir.join("proj.marker.iter.md"),
            "---\nname: P\nlevel: project\nreqs: \"docs/requirements\"\n---\nbody",
        )
        .unwrap();
        assert_eq!(reqs_dir(&dir, &dir), dir.join("docs/requirements"));
        // Engine home and code root can differ — the override rides the code root.
        let engine = tmpdir("override-engine");
        assert_eq!(reqs_dir(&engine, &dir), dir.join("docs/requirements"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&engine);
    }

    #[test]
    fn reqs_dir_prefers_project_level_marker() {
        let dir = tmpdir("prefer");
        // A stray non-project marker at root also carrying reqs: loses to the project one.
        std::fs::write(dir.join("aaa.marker.iter.md"), "---\nlevel: component\nreqs: wrong\n---\n").unwrap();
        std::fs::write(dir.join("zzz.marker.iter.md"), "---\nlevel: project\nreqs: right\n---\n").unwrap();
        assert_eq!(reqs_dir(&dir, &dir), dir.join("right"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reqs_placeholder_and_file_listing() {
        let dir = tmpdir("placeholder");
        let reqs = dir.join(".iter/reqs");
        std::fs::create_dir_all(&reqs).unwrap();
        std::fs::write(reqs.join("bizreq.iter.md"), "BR-1").unwrap();
        std::fs::write(reqs.join("techreq.iter.md"), "TR-1").unwrap();
        std::fs::write(reqs.join("notes.txt"), "not markdown").unwrap();
        let listed = reqs_files(&reqs);
        assert_eq!(listed.len(), 2, "only markdown files list: {:?}", listed);
        let (files, warnings) = resolve(&["{reqs}/*.iter.md".to_string()], &dir, &dir, &reqs);
        assert_eq!(files.len(), 2, "{{reqs}} glob resolves: {:?}", warnings);
        assert!(warnings.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
