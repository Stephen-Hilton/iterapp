use std::path::{Path, PathBuf};

/// Resolve a work item's context patterns into concrete files, deterministically.
/// Substitutes `{codepath}` and `~`, resolves relative patterns against the project
/// root, expands globs. Returns (files, warnings) — a pattern that matches nothing is
/// a warning, never an error.
pub fn resolve(patterns: &[String], codepath: &Path, project_root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for pattern in patterns {
        let mut p = pattern.replace("{codepath}", &codepath.to_string_lossy());
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
        let patterns = vec!["./bizreq.md".to_string(), "./*req.md".to_string(), "./missing.md".to_string()];
        let (files, warnings) = resolve(&patterns, &root, &root);
        assert_eq!(files.len(), 2, "bizreq + techreq, deduped: {:?}", files);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.md"));
    }

    #[test]
    fn codepath_substitution() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample");
        let codepath = root.join("test");
        let patterns = vec!["{codepath}/testgroups.iter.md".to_string()];
        let (files, warnings) = resolve(&patterns, &codepath, &root);
        assert_eq!(files.len(), 1);
        assert!(warnings.is_empty());
    }
}
