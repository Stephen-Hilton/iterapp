//! The project as the local-file verbs see it: a topdir and its main.iter.md.
//! (V2 also read `.iter/config.iter.json` for topdir/mainfile; V3 knows both
//! from the engine — ITER_TOPDIR / ITER_MAINFILE — or from the command line.)

use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::placeholders::Vars;

/// The glob (relative to each scan dir) that finds node files.
pub const ITERGLOB: &str = "**/*.iter.md";

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
    /// File globs ALWAYS loaded into new agent context.
    pub globalcontextfiles: Vec<String>,
    /// The main.iter.md body — the high-level project description.
    pub body: String,
}

/// The resolved project: topdir + main.iter.md, with the placeholder vars
/// every downstream expansion starts from.
#[derive(Debug, Clone)]
pub struct Project {
    /// the checkout root (V3 topdir); `root` kept as the V2 field name
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub topdir: PathBuf,
    pub mainfile: PathBuf,
    pub interfacedir: PathBuf,
    pub usecasedir: PathBuf,
    pub scandirs: Vec<PathBuf>,
}

/// The main file for a topdir: `$ITER_MAINFILE` when the engine exported it,
/// else `<topdir>/main.iter.md`.
pub fn default_mainfile(topdir: &Path) -> PathBuf {
    if let Ok(m) = std::env::var("ITER_MAINFILE") {
        if !m.trim().is_empty() {
            return PathBuf::from(m.trim());
        }
    }
    topdir.join("main.iter.md")
}

impl Project {
    /// Load the project rooted at `topdir` (a missing main.iter.md yields
    /// defaults: the whole topdir is scanned, interfaces/ and usecases/ under it).
    pub fn load(topdir: &Path) -> Project {
        Self::load_with(topdir, &default_mainfile(topdir))
    }

    pub fn load_with(topdir: &Path, mainfile: &Path) -> Project {
        let topdir = topdir.canonicalize().unwrap_or_else(|_| topdir.to_path_buf());
        let mainfile = mainfile.canonicalize().unwrap_or_else(|_| mainfile.to_path_buf());
        let mut vars = Vars::new();
        vars.set("topdir", &format!("{}/", topdir.to_string_lossy()));
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
            config.globalscandirs.iter().map(|d| expand_dir(d, "{topdir}/")).collect()
        };
        Project { root: topdir.clone(), config, topdir, mainfile, interfacedir, usecasedir, scandirs }
    }

    /// The base placeholder vars for this project — every main.iter.md key
    /// becomes a `{placeholder}`, plus the global-dir aliases.
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
        self.topdir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into())
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

/// The scan roots `validate` walks: the project's scandirs.
pub fn scan_roots(topdir: &Path) -> Vec<PathBuf> {
    Project::load(topdir).scandirs
}
