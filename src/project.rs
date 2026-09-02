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
    /// The singular top-level directory. The engine never uses `{topdir}`
    /// directly — it exists as the convenience placeholder other settings'
    /// defaults hang off of. `{thisfiledir}` here = the `.iter/` folder.
    pub topdir: String,
    /// Default context patterns for new work items (was projects.json
    /// `default_context`).
    pub default_context: Vec<String>,
}

/// The ONE glob identifying every iter node file. A constant since the
/// 2026-08-27 settings audit: the dot rule hardcodes the `.iter.md` suffix
/// (`markers::role_of`), so any other glob would collect files the nodetype
/// parser cannot classify — the old `iterglob` setting was illusory choice.
/// (Replaced V1's marker_glob before that.)
pub const ITERGLOB: &str = "**/*.iter.md";

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            mainfile: "{topdir}/main.iter.md".into(),
            topdir: "{thisfiledir}/../".into(),
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
        v.set("iterglob", ITERGLOB);
        v.set("projectname", &self.config.projectname);
        v.set("projectdescription", &self.config.projectdescription);
        v.set("globalinterfacedir", &format!("{}/", self.interfacedir.to_string_lossy()));
        v.set("interfaces", &format!("{}/", self.interfacedir.to_string_lossy()));
        v.set("globalusecasedir", &format!("{}/", self.usecasedir.to_string_lossy()));
        v.set("usecases", &format!("{}/", self.usecasedir.to_string_lossy()));
        // The project's ENGINE HOME — the `.iter/` directory holding agents,
        // prepostwork and .engine. Deliberately distinct from `{iterdir}`,
        // which is where the executable lives: a deployed binary is routinely
        // shared by several projects, so "next to the binary" and "this
        // project's engine home" are different places and must not share a name.
        v.set("dotiter", &format!("{}/", self.root.join(".iter").to_string_lossy()));
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

    /// URL slug (hostname, tab title, favicon tint) — always derived from the
    /// project name; the url_slug override setting was retired 2026-08-27 as
    /// cosmetic-only.
    pub fn slug(&self) -> String {
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

/// Create the global directories the project head DECLARES but nothing ever
/// made: `{globalusecasedir}` and `{globalinterfacedir}`. main.iter.md names
/// them and `/api/projectsettings` resolves them, so the natural move — filing
/// a usecase item whose codepath is the directory the config itself names —
/// used to hand the engine a nonexistent path, which is not a work failure but
/// looked like one 50 times over five days. Idempotent; runs beside
/// `ensure_head_files` at init and at every engine/server start. Returns how
/// many were created.
pub fn ensure_global_dirs(project_root: &Path) -> std::io::Result<usize> {
    let project = Project::load(project_root);
    let mut created = 0;
    for dir in [&project.usecasedir, &project.interfacedir] {
        if !dir.is_dir() {
            std::fs::create_dir_all(dir)?;
            created += 1;
        }
    }
    Ok(created)
}

/// Find the project a command should act on, the way git finds a repo: the
/// given path if it holds `.iter/`, else the nearest ancestor that does.
///
/// The alternative — treating a non-project directory as an empty project —
/// reads as a real answer: `iter status` from a repo root printed "queue: 0
/// open" while 40+ items were open one directory down, and a zero is
/// indistinguishable from a genuinely empty queue. Every caller either gets a
/// real project or an error naming where it looked.
pub fn find_root(start: &Path) -> Result<PathBuf, String> {
    let from = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
    let mut candidate = from.as_path();
    loop {
        if candidate.join(".iter").is_dir() {
            return Ok(candidate.to_path_buf());
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => {
                return Err(format!(
                    "no iter project at {} (no .iter/ directory here or in any parent)",
                    from.display()
                ))
            }
        }
    }
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

    /// Issue 2: the declared directories exist after healing, so an item
    /// whose codepath is one of them dispatches instead of failing forever.
    #[test]
    fn global_dirs_are_created_from_the_declaration() {
        let dir = tmp("globaldirs");
        std::fs::write(
            dir.join("main.iter.md"),
            "---\nprojectname: \"P\"\nglobalusecasedir: \"{topdir}/usecases/\"\nglobalinterfacedir: \"{topdir}/ifaces/\"\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(ensure_global_dirs(&dir).unwrap(), 2);
        assert!(dir.join("usecases").is_dir());
        assert!(dir.join("ifaces").is_dir());
        // Idempotent — a second start creates nothing.
        assert_eq!(ensure_global_dirs(&dir).unwrap(), 0);
        // And the resolved paths now canonicalize to what was created.
        let p = Project::load(&dir);
        assert_eq!(p.usecasedir, dir.canonicalize().unwrap().join("usecases"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue 4: a command run one directory too high finds the project rather
    /// than reporting an empty one, and a directory with no project anywhere
    /// above it is an error, never zeros.
    #[test]
    fn find_root_walks_up_like_git() {
        let dir = tmp("findroot");
        let deep = dir.join("src/nested");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root(&dir).unwrap(), dir.canonicalize().unwrap());
        assert_eq!(find_root(&deep).unwrap(), dir.canonicalize().unwrap(), "walks up to the .iter/ holder");

        // Nothing above a temp dir with no .iter/ anywhere: refused, and the
        // message names where it looked.
        let bare = std::env::temp_dir().join(format!("iter-project-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        let err = find_root(&bare).unwrap_err();
        assert!(err.starts_with("no iter project at"), "{}", err);
        let _ = std::fs::remove_dir_all(&bare);
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
