use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The `{placeholder}` engine (structureV2): ONE substitution style for every
/// path in the DAG. Values live in a `Vars` map; resolution is LAZY — the
/// source strings in .iter.md files are never rewritten, the engine expands
/// them at each use. A key may hold a LIST of values (e.g. `{codedirs}`), in
/// which case a pattern containing it expands cartesian-style, one result per
/// entry.
#[derive(Debug, Clone, Default)]
pub struct Vars {
    map: HashMap<String, Vec<String>>,
}

impl Vars {
    pub fn new() -> Vars {
        Vars::default()
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.map.insert(key.to_string(), vec![value.to_string()]);
    }

    pub fn set_list(&mut self, key: &str, values: &[String]) {
        self.map.insert(key.to_string(), values.to_vec());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).and_then(|v| v.first()).map(|s| s.as_str())
    }

    /// A copy of these vars with the file-relative keys ({thisfilepath},
    /// {thisfilename}, {thisfilestem}, {thisfiledir}) set for `file`. These
    /// always refer to the .iter.md file a pattern appears IN — never the code
    /// it points at — which is what keeps resolution predictable.
    pub fn with_file(&self, file: &Path) -> Vars {
        let mut v = self.clone();
        let fname = file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        v.set("thisfilepath", &file.to_string_lossy());
        v.set("thisfilename", &fname);
        v.set("thisfilestem", &crate::markers::stem_of(&fname));
        if let Some(dir) = file.parent() {
            v.set("thisfiledir", &format!("{}/", dir.to_string_lossy()));
        }
        v
    }

    /// Expand every `{key}` in `pattern`. List-valued keys multiply the result
    /// (cartesian). Unknown keys are left verbatim so the caller can surface
    /// them instead of silently eating the pattern. Path hygiene: `//` → `/`.
    pub fn expand(&self, pattern: &str) -> Vec<String> {
        let mut results = vec![String::new()];
        let mut rest = pattern;
        while let Some(open) = rest.find('{') {
            let Some(close_rel) = rest[open..].find('}') else { break };
            let close = open + close_rel;
            let key = &rest[open + 1..close];
            let literal = &rest[..open];
            let values: Vec<String> = match self.map.get(key) {
                Some(vals) if !vals.is_empty() => vals.clone(),
                _ => vec![format!("{{{}}}", key)], // unknown: keep verbatim
            };
            let mut next = Vec::with_capacity(results.len() * values.len());
            for prefix in &results {
                for val in &values {
                    next.push(format!("{}{}{}", prefix, literal, val));
                }
            }
            results = next;
            rest = &rest[close + 1..];
        }
        for r in &mut results {
            r.push_str(rest);
            // Directories close with a trailing `/`; joining "{dir}/x" against a
            // value that already ends in `/` makes `//` — corrected here.
            while r.contains("//") {
                *r = r.replace("//", "/");
            }
        }
        results
    }

    /// Expand `pattern` then glob every result RECURSIVELY: `a/**/b` matches
    /// with and without intermediate directories (a plain `a/b` too), per the
    /// structureV2 rglob rule. Returns existing FILES, sorted and deduped.
    pub fn expand_files(&self, pattern: &str, base: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for expanded in self.expand(pattern) {
            let abs = absolutize(&expanded, base);
            for variant in rglob_variants(&abs) {
                if variant.contains('*') || variant.contains('?') || variant.contains('[') {
                    if let Ok(paths) = glob::glob(&variant) {
                        for p in paths.flatten() {
                            if p.is_file() && !is_noise(&p) {
                                out.push(p);
                            }
                        }
                    }
                } else {
                    let p = PathBuf::from(&variant);
                    if p.is_file() {
                        out.push(p);
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// Expand `pattern` to existing DIRECTORIES (missing trailing `/` forgiven).
    pub fn expand_dirs(&self, pattern: &str, base: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for expanded in self.expand(pattern) {
            let abs = absolutize(expanded.trim_end_matches('/'), base);
            let p = PathBuf::from(&abs);
            if p.is_dir() {
                out.push(p.canonicalize().unwrap_or(p));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// `~` and relative-path handling shared by every expansion: `~/x` → HOME, a
/// relative pattern resolves against `base`.
fn absolutize(pattern: &str, base: &Path) -> String {
    let mut p = pattern.to_string();
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            p = format!("{}/{}", home.to_string_lossy(), rest);
        }
    }
    if Path::new(&p).is_absolute() {
        p
    } else {
        let joined = base.join(&p);
        joined.to_string_lossy().into_owned()
    }
}

/// The glob crate's `**` wants to own a whole path component; `a/**/b.md`
/// does not reliably match `a/b.md`. structureV2 says every glob is an rglob
/// (zero-or-more directories), so each `**/` pattern also gets a variant with
/// the `**/` removed, and the union is the match set.
fn rglob_variants(pattern: &str) -> Vec<String> {
    let mut out = vec![pattern.to_string()];
    if pattern.contains("**/") {
        let flattened = pattern.replace("**/", "");
        if flattened != pattern {
            out.push(flattened);
        }
    }
    // The dot rule allows a bare, prefix-less nodetype filename
    // (`testgroup.iter.md`), which `*.testgroup.iter.md` cannot match — the
    // `*` cannot absorb the missing dot. Every `*.x` glob therefore also
    // tries the bare `x` form.
    for v in out.clone() {
        if let Some(pos) = v.rfind("/*.") {
            out.push(format!("{}/{}", &v[..pos], &v[pos + 3..]));
        } else if let Some(rest) = v.strip_prefix("*.") {
            out.push(rest.to_string());
        }
    }
    out
}

fn is_noise(p: &Path) -> bool {
    // .iter is the engine home — internal state, personas, temp files; V2
    // never creates node files inside it, so scans never look there.
    p.components().any(|c| matches!(c.as_os_str().to_str(), Some(".git" | "target" | "node_modules" | ".iter")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_expansion_and_double_slash_cleanup() {
        let mut v = Vars::new();
        v.set("topdir", "/proj/");
        assert_eq!(v.expand("{topdir}/src/x.md"), vec!["/proj/src/x.md"]);
        assert_eq!(v.expand("no placeholders"), vec!["no placeholders"]);
        // Unknown keys stay verbatim so callers can warn instead of guessing.
        assert_eq!(v.expand("{nope}/x"), vec!["{nope}/x"]);
    }

    #[test]
    fn list_values_expand_cartesian() {
        let mut v = Vars::new();
        v.set_list("codedirs", &["/a/".into(), "/b/".into()]);
        let got = v.expand("{codedirs}/**/x.code.iter.md");
        assert_eq!(got, vec!["/a/**/x.code.iter.md", "/b/**/x.code.iter.md"]);
    }

    #[test]
    fn file_relative_keys() {
        let v = Vars::new().with_file(Path::new("/p/comp/mylib.code.iter.md"));
        assert_eq!(v.expand("{thisfiledir}test/"), vec!["/p/comp/test/"]);
        assert_eq!(v.expand("{thisfilestem}"), vec!["mylib"]);
        assert_eq!(v.expand("{thisfilename}"), vec!["mylib.code.iter.md"]);
    }

    #[test]
    fn rglob_matches_with_and_without_subdirs() {
        let dir = std::env::temp_dir().join(format!("iter-rglob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.code.iter.md"), "x").unwrap();
        std::fs::write(dir.join("sub/b.code.iter.md"), "x").unwrap();
        let v = Vars::new();
        let pattern = format!("{}/**/*.code.iter.md", dir.display());
        let found = v.expand_files(&pattern, &dir);
        assert_eq!(found.len(), 2, "rglob matches zero AND one level deep: {:?}", found);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
