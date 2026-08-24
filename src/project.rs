use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::placeholders::Vars;

/// structureV2's Project Server: 1 server = 1 project. Two files drive it —
///
/// `.iter/config.iter.json` — set-and-forget SERVER settings; drives the
/// engine, never enters agent context. Replaces `.iter/projects.json` and the
/// pathing half of the old globalsettings.
///
/// `main.iter.md` (anywhere; `mainfile` points at it) — the evolving project
/// definition, FIRST file in every agent context. Its frontmatter keys become
/// `{placeholders}` for downstream nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Path (placeholder-expanded) to the main.iter.md project definition.
    pub mainfile: String,
    /// The ONE glob identifying every iter node file. Replaces marker_glob.
    pub iterglob: String,
    /// The singular top-level directory. The engine never uses `{topdir}`
    /// directly — it exists as the convenience placeholder other settings'
    /// defaults hang off of. `{thisfiledir}` here = the `.iter/` folder.
    pub topdir: String,
    /// Optional URL slug; empty derives from the project name.
    pub url_slug: String,
    /// Default context patterns for new work items (was projects.json
    /// `default_context`).
    pub default_context: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            mainfile: "{topdir}/main.iter.md".into(),
            iterglob: "**/*.iter.md".into(),
            topdir: "{thisfiledir}/../".into(),
            url_slug: String::new(),
            default_context: vec!["{marker}".into(), "{ancestor_markers}".into(), "{interfaces}".into()],
        }
    }
}

/// main.iter.md frontmatter — project settings that evolve with the project.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectConfig {
    pub projectname: String,
    pub projectdescription: String,
    /// Directories the engine scans (with iterglob) for node files.
    pub globalscandirs: Vec<String>,
    /// The global folder of `*.interface.iter.md` files. Alias `{interfaces}`.
    pub globalinterfacedir: String,
    /// The global folder of `*.usecase.iter.md` files. Alias `{usecases}`.
    pub globalusecasedir: String,
    /// File globs ALWAYS loaded into new agent context (absorbs the old
    /// global_bizreq_path / global_techreq_path settings).
    pub globalcontextfiles: Vec<String>,
    /// The main.iter.md body — the high-level project description.
    pub body: String,
}

/// The resolved project: server config + main.iter.md, with the placeholder
/// vars every downstream expansion starts from.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf, // the directory holding .iter/ (what --project points at)
    pub server: ServerConfig,
    pub config: ProjectConfig,
    pub topdir: PathBuf,
    pub mainfile: PathBuf,
    pub interfacedir: PathBuf,
    pub usecasedir: PathBuf,
    pub scandirs: Vec<PathBuf>,
}

pub fn config_path(project_root: &Path) -> PathBuf {
    project_root.join(".iter").join("config.iter.json")
}

impl Project {
    /// Load (and default-fill) the whole project head. Never errors: a missing
    /// config or mainfile yields defaults, so `iter start` in a fresh directory
    /// works and healing can add the stubs.
    pub fn load(project_root: &Path) -> Project {
        let root = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
        let server: ServerConfig = std::fs::read_to_string(config_path(&root))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();

        // {thisfiledir} in config.iter.json = the .iter/ folder itself.
        let iter_dir = root.join(".iter");
        let mut vars = Vars::new();
        vars.set("thisfiledir", &format!("{}/", iter_dir.to_string_lossy()));
        let topdir_raw = vars.expand(&server.topdir).pop().unwrap_or_default();
        let topdir = PathBuf::from(topdir_raw.trim_end_matches('/'));
        let topdir = topdir.canonicalize().unwrap_or(topdir);
        vars.set("topdir", &format!("{}/", topdir.to_string_lossy()));

        let mainfile_raw = vars.expand(&server.mainfile).pop().unwrap_or_default();
        let mainfile = PathBuf::from(mainfile_raw);
        let mainfile = mainfile.canonicalize().unwrap_or(mainfile);

        let config = load_main(&mainfile);
        let expand_dir = |raw: &str, fallback: &str| -> PathBuf {
            let raw = if raw.trim().is_empty() { fallback } else { raw };
            let s = vars.expand(raw).pop().unwrap_or_default();
            let p = PathBuf::from(s.trim_end_matches('/'));
            p.canonicalize().unwrap_or(p)
        };
        let interfacedir = expand_dir(&config.globalinterfacedir, "{topdir}/interfaces/");
        let usecasedir = expand_dir(&config.globalusecasedir, "{topdir}/usecases/");
        let scandirs: Vec<PathBuf> = if config.globalscandirs.is_empty() {
            vec![topdir.clone()]
        } else {
            config
                .globalscandirs
                .iter()
                .map(|d| expand_dir(d, "{topdir}/"))
                .collect()
        };

        Project { root, server, config, topdir, mainfile, interfacedir, usecasedir, scandirs }
    }

    /// The base placeholder vars for this project — config-derived keys (every
    /// key in both head files becomes a `{placeholder}`), the engine keys, and
    /// the global-dir aliases. File-relative keys are added per file via
    /// `Vars::with_file`.
    pub fn vars(&self) -> Vars {
        let mut v = Vars::new();
        v.set("topdir", &format!("{}/", self.topdir.to_string_lossy()));
        v.set("mainfile", &self.mainfile.to_string_lossy());
        v.set("iterglob", &self.server.iterglob);
        v.set("projectname", &self.config.projectname);
        v.set("projectdescription", &self.config.projectdescription);
        v.set("globalinterfacedir", &format!("{}/", self.interfacedir.to_string_lossy()));
        v.set("interfaces", &format!("{}/", self.interfacedir.to_string_lossy()));
        v.set("globalusecasedir", &format!("{}/", self.usecasedir.to_string_lossy()));
        v.set("usecases", &format!("{}/", self.usecasedir.to_string_lossy()));
        if let Ok(exe) = std::env::current_exe() {
            v.set("iter", &exe.to_string_lossy());
            if let Some(dir) = exe.parent() {
                v.set("iterdir", &format!("{}/", dir.to_string_lossy()));
            }
        }
        v
    }

    /// Every file the project pins into ALL new agent context: main.iter.md
    /// first, then the resolved `globalcontextfiles` globs.
    pub fn context_files(&self) -> Vec<PathBuf> {
        let vars = self.vars();
        let mut out = Vec::new();
        if self.mainfile.is_file() {
            out.push(self.mainfile.clone());
        }
        for pattern in &self.config.globalcontextfiles {
            out.extend(vars.expand_files(pattern, &self.topdir));
        }
        out.dedup();
        out
    }

    pub fn projectname(&self) -> String {
        if !self.config.projectname.trim().is_empty() {
            return self.config.projectname.trim().to_string();
        }
        self.topdir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into())
    }

    pub fn slug(&self) -> String {
        if !self.server.url_slug.trim().is_empty() {
            return self.server.url_slug.trim().to_string();
        }
        let cleaned: String = self
            .projectname()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let s = cleaned.trim_matches('-').to_string();
        if s.is_empty() { "project".into() } else { s }
    }
}

fn load_main(mainfile: &Path) -> ProjectConfig {
    let Ok(text) = std::fs::read_to_string(mainfile) else { return ProjectConfig::default() };
    let front = crate::markers::parse_front(&text);
    ProjectConfig {
        projectname: front.scalar("projectname"),
        projectdescription: front.scalar("projectdescription"),
        globalscandirs: front.list("globalscandirs"),
        globalinterfacedir: front.scalar("globalinterfacedir"),
        globalusecasedir: front.scalar("globalusecasedir"),
        globalcontextfiles: front.list("globalcontextfiles"),
        body: front.body,
    }
}

/// Healing for the two head files: config.iter.json in .iter/, and a stub
/// main.iter.md at the configured mainfile path. Never overwrites; returns
/// how many were created.
pub fn ensure_head_files(project_root: &Path) -> std::io::Result<usize> {
    let mut created = 0;
    let cfg_path = config_path(project_root);
    if !cfg_path.exists() {
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&ServerConfig::default()).expect("serializes");
        std::fs::write(&cfg_path, text)?;
        created += 1;
    }
    let project = Project::load(project_root);
    if !project.mainfile.exists() {
        if let Some(parent) = project.mainfile.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let name = project.projectname();
        std::fs::write(
            &project.mainfile,
            format!(
                "---\nprojectname: \"{name}\"\nprojectdescription: \"<one line on what this project is>\"\nglobalscandirs: [\"{{topdir}}/\"]\nglobalinterfacedir: \"{{topdir}}/interfaces/\"\nglobalusecasedir: \"{{topdir}}/usecases/\"\nglobalcontextfiles: []\n---\n\n# {name}\n\n<The guiding high-level vision: what this project is, who it serves, and the\nshape of the build. This body is the FIRST content loaded into every agent\ncontext — keep it current.>\n",
                name = name
            ),
        )?;
        created += 1;
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iter-project-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".iter")).unwrap();
        dir
    }

    #[test]
    fn defaults_resolve_topdir_to_parent_of_dot_iter() {
        let dir = tmp("defaults");
        let p = Project::load(&dir);
        assert_eq!(p.topdir, dir.canonicalize().unwrap());
        assert!(p.mainfile.ends_with("main.iter.md"));
        assert_eq!(p.scandirs, vec![p.topdir.clone()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heal_creates_config_and_main_stub_once() {
        let dir = tmp("heal");
        assert_eq!(ensure_head_files(&dir).unwrap(), 2);
        assert!(config_path(&dir).is_file());
        assert!(dir.join("main.iter.md").is_file());
        // Idempotent, never overwrites.
        std::fs::write(dir.join("main.iter.md"), "---\nprojectname: \"Real\"\n---\nreal body\n").unwrap();
        assert_eq!(ensure_head_files(&dir).unwrap(), 0);
        let p = Project::load(&dir);
        assert_eq!(p.projectname(), "Real");
        assert_eq!(p.slug(), "real");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn main_frontmatter_drives_dirs_and_context() {
        let dir = tmp("mainfm");
        std::fs::create_dir_all(dir.join("ifaces")).unwrap();
        std::fs::create_dir_all(dir.join("reqs")).unwrap();
        std::fs::write(dir.join("reqs/big.bizreq.iter.md"), "BR-1").unwrap();
        std::fs::write(
            dir.join("main.iter.md"),
            "---\nprojectname: \"P\"\nglobalinterfacedir: \"{topdir}/ifaces/\"\nglobalcontextfiles: [\"{topdir}/reqs/*.iter.md\"]\n---\nbody\n",
        )
        .unwrap();
        let p = Project::load(&dir);
        assert!(p.interfacedir.ends_with("ifaces"));
        let ctx = p.context_files();
        assert_eq!(ctx.len(), 2, "main.iter.md first, then the req glob: {:?}", ctx);
        assert!(ctx[0].ends_with("main.iter.md"));
        assert!(ctx[1].ends_with("big.bizreq.iter.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topdir_can_sit_above_the_engine_home() {
        // The pdy-dev shape: .iter lives in <top>/devops/.iter, topdir = <top>.
        let top = std::env::temp_dir().join(format!("iter-project-above-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        let root = top.join("devops");
        std::fs::create_dir_all(root.join(".iter")).unwrap();
        std::fs::write(
            config_path(&root),
            r#"{ "topdir": "{thisfiledir}/../../", "mainfile": "{topdir}/main.iter.md" }"#,
        )
        .unwrap();
        let p = Project::load(&root);
        assert_eq!(p.topdir, top.canonicalize().unwrap());
        assert_eq!(p.mainfile, top.canonicalize().unwrap().join("main.iter.md"));
        let _ = std::fs::remove_dir_all(&top);
    }
}
