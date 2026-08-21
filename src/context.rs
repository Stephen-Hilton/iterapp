use std::path::{Path, PathBuf};

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

    /// The shipped reference project (sampleV1/), scaffolded by the current
    /// engine — the fixture these tests read is a real iterapp project, so a
    /// change that breaks real projects breaks the suite too.
    fn sample() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("sampleV1")
    }

    #[test]
    fn resolves_sample_context() {
        let root = sample();
        let reqs = crate::config::reqs_dir(&root, &crate::config::Config::default());
        let patterns = vec!["./reqs/bizreq.iter.md".to_string(), "./reqs/*req.iter.md".to_string(), "./missing.md".to_string()];
        let (files, warnings) = resolve(&patterns, &root, &root, &reqs);
        assert_eq!(files.len(), 2, "bizreq + techreq, deduped: {:?}", files);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.md"));
    }

    #[test]
    fn codepath_substitution() {
        let root = sample();
        let codepath = root.join("ledger/cli/parse/test");
        let patterns = vec!["{codepath}/testgroup.iter.md".to_string()];
        let (files, warnings) = resolve(&patterns, &codepath, &root, &root.join("reqs"));
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
    fn reqs_placeholder_resolves() {
        let dir = tmpdir("placeholder");
        let reqs = dir.join("reqs");
        std::fs::create_dir_all(&reqs).unwrap();
        std::fs::write(reqs.join("bizreq.iter.md"), "BR-1").unwrap();
        std::fs::write(reqs.join("techreq.iter.md"), "TR-1").unwrap();
        let (files, warnings) = resolve(&["{reqs}/*.iter.md".to_string()], &dir, &dir, &reqs);
        assert_eq!(files.len(), 2, "{{reqs}} glob resolves: {:?}", warnings);
        assert!(warnings.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
