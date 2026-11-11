//! Repository discovery and the resolved context every command runs in.

use std::path::{Component, Path, PathBuf};

use crate::config::{CONFIG_FILE, Config};
use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::ignore::{IGNORE_FILE, Ignore};
use crate::paths::{RelPath, Scope, expand_tilde};
use crate::source::{Discovery, Source, discover};

/// Values from the command line that select the repository and targets.
#[derive(Debug, Clone, Default)]
pub struct GlobalOpts {
    pub repo: Option<PathBuf>,
    pub target: Option<PathBuf>,
    pub root_target: Option<PathBuf>,
}

/// Process environment, captured once so that tests can supply their own.
#[derive(Debug, Clone)]
pub struct Env {
    pub cwd: PathBuf,
    pub home: PathBuf,
    /// `RUSTOW_REPO`.
    pub repo: Option<PathBuf>,
    /// `XDG_STATE_HOME`, used only when absolute.
    pub xdg_state_home: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Result<Env> {
        let cwd = std::env::current_dir().map_err(|e| Error::Failed(format!("current directory: {e}")))?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute())
            .ok_or_else(|| Error::Usage("HOME is not set to an absolute path".into()))?;
        let var = |name| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Ok(Env {
            cwd,
            home,
            repo: var("RUSTOW_REPO"),
            xdg_state_home: var("XDG_STATE_HOME"),
        })
    }
}

/// Everything a command needs to know about the repository and targets.
pub struct Context {
    /// Canonical repository root.
    pub root: PathBuf,
    pub config: Config,
    pub ignore: Ignore,
    /// Canonical home of the invoking user (for `~` expansion).
    pub home: PathBuf,
    /// Canonical target for home-scope sources.
    pub home_target: PathBuf,
    /// Canonical target for `.rustowroot/` sources.
    pub root_target: PathBuf,
    /// Short hash of `root`.
    pub repo_id: String,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Messages for the user about how the context was chosen.
    pub notices: Vec<String>,
}

impl Context {
    pub fn open(opts: &GlobalOpts, env: &Env) -> Result<Context> {
        let mut notices = Vec::new();
        let home = canonical_dir(&env.home, "HOME")?;
        let root = find_root(opts, env, &mut notices)?;
        if root.parent().is_none() {
            return Err(Error::Usage(
                "refusing to use the filesystem root as the repository".into(),
            ));
        }
        let config = Config::load(&root)?;
        let target = |flag: &Option<PathBuf>, configured: &str, what: &str| match flag {
            Some(p) => canonical_dir(&env.cwd.join(p), what),
            None => canonical_dir(&root.join(expand_tilde(configured, &home)), what),
        };
        let home_target = target(&opts.target, &config.defaults.target, "target")?;
        let root_target = target(&opts.root_target, &config.defaults.root_target, "root target")?;
        for (dir, what) in [(&home_target, "target"), (&root_target, "root target")] {
            if dir.starts_with(&root) {
                return Err(Error::Usage(format!(
                    "{what} {} is inside the repository {}",
                    dir.display(),
                    root.display()
                )));
            }
        }

        let InternalDirs {
            cache_dir,
            state_dir,
            inside,
        } = InternalDirs::resolve(&root, &home, &config, env)?;
        let ignore = Ignore::load(&root, inside)?;
        if root_target != Path::new("/") {
            notices.push(format!(
                "system files (.rustowroot/) deploy under {}",
                root_target.display()
            ));
        }
        let repo_id = Hash::of(root.as_os_str().as_encoded_bytes()).short().to_owned();
        Ok(Context {
            root,
            config,
            ignore,
            home,
            home_target,
            root_target,
            repo_id,
            cache_dir,
            state_dir,
            notices,
        })
    }

    pub fn discover(&self) -> Result<Discovery> {
        discover(&self.root, &self.ignore, &self.config)
    }

    pub fn target_root(&self, scope: Scope) -> &Path {
        match scope {
            Scope::Home => &self.home_target,
            Scope::Root => &self.root_target,
        }
    }

    /// Absolute destination of a source.
    pub fn dest_path(&self, source: &Source) -> PathBuf {
        self.target_root(source.scope).join(source.dest.as_path())
    }

    /// Short id of a scope and its target root, for cache paths.
    pub fn scope_id(&self, scope: Scope) -> String {
        let key = format!("{}:{}", scope.name(), self.target_root(scope).display());
        Hash::of(key.as_bytes()).short().to_owned()
    }

    /// Sources selected by repository-relative `args`; all when empty. A
    /// directory selects its subtree. Reserved, ignored and unknown paths
    /// are errors.
    pub fn select<'a>(&self, discovery: &'a Discovery, args: &[String]) -> Result<Vec<&'a Source>> {
        let mut wanted = Vec::new();
        for arg in args.iter().filter(|a| !matches!(a.as_str(), "." | "./")) {
            let rel = RelPath::parse(arg)?;
            let is_dir = self.root.join(rel.as_path()).is_dir();
            if self.ignore.is_reserved(&rel, is_dir) {
                return Err(Error::Usage(format!(
                    "'{rel}' is reserved by rustow and is never deployed"
                )));
            }
            if self.ignore.is_ignored(&rel, is_dir) {
                return Err(Error::Usage(format!("'{rel}' is ignored by {IGNORE_FILE}")));
            }
            if !discovery.sources.iter().any(|s| s.path.starts_with(&rel)) {
                return Err(Error::Usage(format!("'{rel}' does not match any deployable source")));
            }
            wanted.push(rel);
        }
        let all = wanted.is_empty();
        Ok(discovery
            .sources
            .iter()
            .filter(|s| all || wanted.iter().any(|w| s.path.starts_with(w)))
            .collect())
    }
}

fn canonical_dir(path: &Path, what: &str) -> Result<PathBuf> {
    let dir = path
        .canonicalize()
        .map_err(|e| Error::Usage(format!("{what} {}: {e}", path.display())))?;
    if !dir.is_dir() {
        return Err(Error::Usage(format!("{what} {} is not a directory", dir.display())));
    }
    Ok(dir)
}

/// `--repo`, then `RUSTOW_REPO`, then the nearest ancestor of the working
/// directory with `.rustow.toml` or `.rustowignore`, then the working directory.
pub fn find_root(opts: &GlobalOpts, env: &Env, notices: &mut Vec<String>) -> Result<PathBuf> {
    if let Some(p) = &opts.repo {
        return canonical_dir(&env.cwd.join(p), "--repo");
    }
    if let Some(p) = &env.repo {
        return canonical_dir(&env.cwd.join(p), "RUSTOW_REPO");
    }
    let cwd = canonical_dir(&env.cwd, "current directory")?;
    if let Some(dir) = cwd
        .ancestors()
        .find(|d| d.join(CONFIG_FILE).exists() || d.join(IGNORE_FILE).exists())
    {
        return Ok(dir.to_path_buf());
    }
    notices.push(format!(
        "no {CONFIG_FILE} or {IGNORE_FILE} found; using the current directory {} as the repository",
        cwd.display()
    ));
    Ok(cwd)
}

/// Cache and state directories of a repository.
pub struct InternalDirs {
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Those of them inside the repository, repository-relative.
    pub inside: Vec<RelPath>,
}

impl InternalDirs {
    /// Relative values resolve against the repository, never the working directory.
    pub fn resolve(root: &Path, home: &Path, config: &Config, env: &Env) -> Result<InternalDirs> {
        let resolve = |value: &str| normalize(&root.join(expand_tilde(value, home)));
        let cache_dir = resolve(&config.defaults.cache_dir);
        let state_dir = match (&config.defaults.state_dir, &env.xdg_state_home) {
            (Some(value), _) => resolve(value),
            (None, Some(xdg)) if xdg.is_absolute() => xdg.join("rustow"),
            (None, _) => home.join(".local/state/rustow"),
        };
        let inside = internal_dirs(root, &[("cache_dir", &cache_dir), ("state_dir", &state_dir)])?;
        Ok(InternalDirs {
            cache_dir,
            state_dir,
            inside,
        })
    }
}

/// Repository-relative paths of configured internal dirs that lie inside
/// the repository. Rejects layouts that cannot be isolated from sources.
fn internal_dirs(root: &Path, dirs: &[(&str, &PathBuf)]) -> Result<Vec<RelPath>> {
    let mut out = Vec::new();
    for (name, dir) in dirs {
        let bad = |why: &str| Error::Config(format!("{CONFIG_FILE}: {name} {}: {why}", dir.display()));
        if root.starts_with(dir) {
            return Err(bad("must not be the repository or one of its parents"));
        }
        let Ok(rel) = dir.strip_prefix(root) else {
            continue;
        };
        let rel = RelPath::parse(&rel.to_string_lossy()).map_err(|e| bad(&e.to_string()))?;
        let first = rel.components().next().unwrap_or_default();
        if first == crate::ignore::ROOT_DIR || first == ".git" {
            return Err(bad("must not be inside .rustowroot or .git"));
        }
        out.push(rel);
    }
    Ok(out)
}

/// Resolves `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fx {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        env: Env,
    }

    /// base/home (HOME), base/home/dot (repo with .rustow.toml), base/root.
    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        for d in ["home/dot/.config/nvim", "root"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        fs::write(base.join("home/dot/.rustow.toml"), "version = 1\n").unwrap();
        fs::write(base.join("home/dot/.bashrc"), "x").unwrap();
        fs::write(base.join("home/dot/.config/nvim/init.lua"), "x").unwrap();
        let env = Env {
            cwd: base.join("home/dot"),
            home: base.join("home"),
            repo: None,
            xdg_state_home: None,
        };
        Fx { _tmp: tmp, base, env }
    }

    fn open(fx: &Fx, opts: GlobalOpts) -> Result<Context> {
        Context::open(&opts, &fx.env)
    }

    fn names(sel: &[&Source]) -> Vec<String> {
        sel.iter().map(|s| s.path.to_string()).collect()
    }

    #[test]
    fn discovers_root_from_subdir_by_marker() {
        let mut f = fx();
        f.env.cwd = f.base.join("home/dot/.config/nvim");
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        assert_eq!(ctx.root, f.base.join("home/dot"));
        assert_eq!(ctx.home_target, f.base.join("home"));
        assert!(ctx.notices.is_empty());
        assert_eq!(ctx.state_dir, f.base.join("home/.local/state/rustow"));
        assert_eq!(ctx.cache_dir, f.base.join("home/dot/.rustowcache"));
    }

    #[test]
    fn rustowignore_is_also_a_marker() {
        let mut f = fx();
        fs::create_dir_all(f.base.join("other/sub")).unwrap();
        fs::write(f.base.join("other/.rustowignore"), "").unwrap();
        f.env.cwd = f.base.join("other/sub");
        assert_eq!(open(&f, GlobalOpts::default()).unwrap().root, f.base.join("other"));
    }

    #[test]
    fn repo_flag_wins_over_env_and_cwd() {
        let mut f = fx();
        fs::create_dir_all(f.base.join("other")).unwrap();
        f.env.repo = Some(f.base.join("root"));
        let opts = GlobalOpts {
            repo: Some(f.base.join("other")),
            ..Default::default()
        };
        assert_eq!(open(&f, opts).unwrap().root, f.base.join("other"));
    }

    #[test]
    fn env_repo_used() {
        let mut f = fx();
        fs::create_dir_all(f.base.join("other")).unwrap();
        f.env.repo = Some(f.base.join("other"));
        assert_eq!(open(&f, GlobalOpts::default()).unwrap().root, f.base.join("other"));
    }

    #[test]
    fn cwd_fallback_adds_notice() {
        let mut f = fx();
        fs::create_dir_all(f.base.join("plain")).unwrap();
        f.env.cwd = f.base.join("plain");
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        assert_eq!(ctx.root, f.base.join("plain"));
        assert!(ctx.notices[0].contains("current directory"), "{:?}", ctx.notices);
    }

    #[test]
    fn missing_repo_flag_dir_is_error() {
        let f = fx();
        let opts = GlobalOpts {
            repo: Some(f.base.join("nope")),
            ..Default::default()
        };
        assert!(open(&f, opts).is_err());
    }

    #[test]
    fn target_inside_repo_rejected() {
        let f = fx();
        let opts = GlobalOpts {
            target: Some(f.base.join("home/dot/.config")),
            ..Default::default()
        };
        let e = open(&f, opts).err().unwrap().to_string();
        assert!(e.contains("inside the repository"), "{e}");
    }

    #[test]
    fn repo_equal_target_rejected() {
        let f = fx();
        let opts = GlobalOpts {
            target: Some(f.base.join("home/dot")),
            ..Default::default()
        };
        assert!(open(&f, opts).is_err());
    }

    #[test]
    fn filesystem_root_repo_rejected() {
        let f = fx();
        let opts = GlobalOpts {
            repo: Some(PathBuf::from("/")),
            ..Default::default()
        };
        assert!(open(&f, opts).is_err());
    }

    #[test]
    fn relative_cache_dir_resolves_against_repo_not_cwd() {
        let mut f = fx();
        fs::write(
            f.base.join("home/dot/.rustow.toml"),
            "version = 1\n[defaults]\ncache_dir = \"var/c\"\n",
        )
        .unwrap();
        f.env.cwd = f.base.join("home/dot/.config");
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        assert_eq!(ctx.cache_dir, f.base.join("home/dot/var/c"));
        // The configured cache is excluded, and so is the default one.
        assert!(ctx.ignore.is_reserved(&RelPath::parse("var/c/x").unwrap(), false));
        assert!(
            ctx.ignore
                .is_reserved(&RelPath::parse(".rustowcache/x").unwrap(), false)
        );
    }

    #[test]
    fn cache_dir_at_repo_root_rejected() {
        let f = fx();
        fs::write(
            f.base.join("home/dot/.rustow.toml"),
            "version = 1\n[defaults]\ncache_dir = \".\"\n",
        )
        .unwrap();
        let e = open(&f, GlobalOpts::default()).err().unwrap().to_string();
        assert!(e.contains("cache_dir"), "{e}");
    }

    #[test]
    fn state_dir_prefers_xdg_and_config() {
        let mut f = fx();
        f.env.xdg_state_home = Some(f.base.join("xdg"));
        assert_eq!(
            open(&f, GlobalOpts::default()).unwrap().state_dir,
            f.base.join("xdg/rustow")
        );
        fs::write(
            f.base.join("home/dot/.rustow.toml"),
            "version = 1\n[defaults]\nstate_dir = \"~/st\"\n",
        )
        .unwrap();
        assert_eq!(
            open(&f, GlobalOpts::default()).unwrap().state_dir,
            f.base.join("home/st")
        );
    }

    #[test]
    fn select_all_and_dir_prefix() {
        let f = fx();
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        let d = ctx.discover().unwrap();
        assert_eq!(
            names(&ctx.select(&d, &[]).unwrap()),
            [".bashrc", ".config/nvim/init.lua"]
        );
        assert_eq!(
            names(&ctx.select(&d, &[".config/".into()]).unwrap()),
            [".config/nvim/init.lua"]
        );
        assert_eq!(
            names(&ctx.select(&d, &["./.bashrc".into(), ".bashrc".into()]).unwrap()),
            [".bashrc"]
        );
    }

    #[test]
    fn select_ignored_reserved_or_unknown_errors() {
        let f = fx();
        fs::write(f.base.join("home/dot/.rustowignore"), "*.bak\n").unwrap();
        fs::write(f.base.join("home/dot/x.bak"), "").unwrap();
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        let d = ctx.discover().unwrap();
        for (arg, word) in [
            ("x.bak", "ignored"),
            (".rustowcache/x", "reserved"),
            (".rustow.toml", "reserved"),
            ("nope", "does not match"),
            ("../etc", "'..'"),
        ] {
            let e = ctx.select(&d, &[arg.into()]).unwrap_err().to_string();
            assert!(e.contains(word), "{arg}: {e}");
        }
    }

    #[test]
    fn root_scope_dest_uses_root_target() {
        let f = fx();
        fs::create_dir_all(f.base.join("home/dot/.rustowroot/etc")).unwrap();
        fs::write(f.base.join("home/dot/.rustowroot/etc/x.conf"), "").unwrap();
        let opts = GlobalOpts {
            root_target: Some(f.base.join("root")),
            ..Default::default()
        };
        let ctx = open(&f, opts).unwrap();
        let d = ctx.discover().unwrap();
        let s = ctx.select(&d, &[".rustowroot/etc".into()]).unwrap();
        assert_eq!(ctx.dest_path(s[0]), f.base.join("root/etc/x.conf"));
        assert_ne!(ctx.scope_id(Scope::Root), ctx.scope_id(Scope::Home));
    }

    #[test]
    fn missing_target_is_error() {
        let f = fx();
        let opts = GlobalOpts {
            root_target: Some(f.base.join("no-root")),
            ..Default::default()
        };
        assert!(open(&f, opts).is_err());
    }
}
