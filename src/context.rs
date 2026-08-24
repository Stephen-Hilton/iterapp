use std::path::{Path, PathBuf};

/// Resolve a work item's context patterns into concrete files, deterministically.
/// Substitutes `{codepath}` (the item's resolved primary codepath), `{topdir}`
/// (the project's top directory — structureV2's one head), and `~`; resolves
/// relative patterns against `{topdir}`; expands globs. Returns
/// (files, warnings) — a pattern that matches nothing is a warning, never an error.
pub fn resolve(patterns: &[String], codepath: &Path, topdir: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for pattern in patterns {
        let mut p = pattern
            .replace("{codepath}", &codepath.to_string_lossy())
            .replace("{topdir}", &topdir.to_string_lossy());
        if let Some(rest) = p.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                p = format!("{}/{}", home.to_string_lossy(), rest);
            }
        }
        while p.contains("//") {
            p = p.replace("//", "/");
        }
        let abs = if Path::new(&p).is_absolute() {
            p.clone()
        } else {
            topdir.join(&p).to_string_lossy().into_owned()
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

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-ctx-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_relative_globs_against_topdir() {
        let dir = tmpdir("rel");
        std::fs::create_dir_all(dir.join("reqs")).unwrap();
        std::fs::write(dir.join("reqs/a.bizreq.iter.md"), "BR").unwrap();
        std::fs::write(dir.join("reqs/b.techreq.iter.md"), "TR").unwrap();
        let patterns = vec!["./reqs/a.bizreq.iter.md".to_string(), "./reqs/*req.iter.md".to_string(), "./missing.md".to_string()];
        let (files, warnings) = resolve(&patterns, &dir, &dir);
        assert_eq!(files.len(), 2, "both reqs, deduped: {:?}", files);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codepath_and_topdir_placeholders() {
        let dir = tmpdir("ph");
        std::fs::create_dir_all(dir.join("comp/test")).unwrap();
        std::fs::write(dir.join("comp/test/x.testgroup.iter.md"), "tg").unwrap();
        std::fs::write(dir.join("main.iter.md"), "m").unwrap();
        let codepath = dir.join("comp");
        let (files, warnings) =
            resolve(&["{codepath}/test/x.testgroup.iter.md".to_string(), "{topdir}/main.iter.md".to_string()], &codepath, &dir);
        assert_eq!(files.len(), 2, "{:?}", warnings);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
