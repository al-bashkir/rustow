//! Repository discovery and the resolved context every command runs in.

use std::path::{Path, PathBuf};

use crate::config::{CONFIG_FILE, Config};
use crate::error::{Error, Result};
use crate::hash::Hash;
use crate::ignore::{IGNORE_FILE, Ignore};
use crate::paths::{RelPath, Scope, expand_tilde, normalize};
use crate::source::{Discovery, Skipped, Source, discover};
use crate::template::Host;

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
    /// Passphrase prompts allowed (`--interactive`).
    pub interactive: bool,
    /// Private keys, loaded on first use.
    pub keys: crate::crypto::Keys,
}

impl Context {
    pub fn open(opts: &GlobalOpts, env: &Env) -> Result<Context> {
        let mut notices = Vec::new();
        let home = canonical_dir(&env.home, "HOME")?;
        let root = find_root(opts, env, &mut notices, false)?;
        if root.parent().is_none() {
            return Err(Error::Usage(
                "refusing to use the filesystem root as the repository".into(),
            ));
        }
        let config = Config::load(&root)?;
        let (home_target, root_target) = resolve_targets(opts, env, &root, &config, &home)?;

        let InternalDirs {
            cache_dir,
            state_dir,
            inside,
            private_cache,
        } = InternalDirs::resolve(&root, &home, &config, env)?;
        let ignore = Ignore::load(&root, inside)?;
        notices.extend(setup_notices(
            private_cache.then_some(cache_dir.as_path()),
            &root_target,
        ));
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
            interactive: false,
            keys: Default::default(),
        })
    }

    pub fn discover(&self) -> Result<Discovery> {
        discover(&self.root, &self.ignore, &self.config, &Host::current())
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
    /// are errors, and so is a path that names a skipped file.
    pub fn select<'a>(&self, discovery: &'a Discovery, args: &[String]) -> Result<Vec<&'a Source>> {
        let wanted = self.wanted(discovery, args)?;
        Ok(discovery.sources.iter().filter(|s| covers(&wanted, &s.path)).collect())
    }

    /// Skipped files and entries that `args` select, as for `select`.
    pub fn skipped<'a>(&self, discovery: &'a Discovery, args: &[String]) -> Result<Vec<&'a Skipped>> {
        let wanted = self.wanted(discovery, args)?;
        Ok(discovery.skipped.iter().filter(|s| covers(&wanted, &s.path)).collect())
    }

    /// The checked path prefixes of `args`; none selects everything.
    fn wanted(&self, discovery: &Discovery, args: &[String]) -> Result<Vec<RelPath>> {
        args.iter()
            .filter(|a| !matches!(a.as_str(), "." | "./"))
            .map(|arg| self.check_arg(discovery, RelPath::parse(arg)?))
            .collect()
    }

    /// One path argument must select a source, or only skipped files below a directory.
    fn check_arg(&self, discovery: &Discovery, rel: RelPath) -> Result<RelPath> {
        let is_dir = self.root.join(rel.as_path()).is_dir();
        if self.ignore.is_reserved(&rel, is_dir) {
            return Err(Error::Usage(format!(
                "'{rel}' is reserved by rustow and is never deployed"
            )));
        }
        if self.ignore.is_ignored(&rel, is_dir) {
            return Err(Error::Usage(format!("'{rel}' is ignored by {IGNORE_FILE}")));
        }
        // An inactive template is not for this host: selecting it is not an error.
        let mut paths = (discovery.sources.iter().map(|s| &s.path)).chain(discovery.inactive.iter().map(|s| &s.path));
        if paths.any(|p| p.starts_with(&rel)) {
            return Ok(rel);
        }
        if let Some(s) = discovery.skipped.iter().find(|s| s.path == rel) {
            return Err(Error::Usage(format!("source '{rel}': {}", s.reason)));
        }
        if !discovery.skipped.iter().any(|s| s.path.starts_with(&rel)) {
            return Err(Error::Usage(format!("'{rel}' does not match any deployable source")));
        }
        Ok(rel)
    }
}

/// `path` lies at or below one of `wanted`, or `wanted` is empty.
fn covers(wanted: &[RelPath], path: &RelPath) -> bool {
    wanted.is_empty() || wanted.iter().any(|w| path.starts_with(w))
}

/// Canonical home and root targets (flag, else configuration); neither may lie inside the repository.
fn resolve_targets(
    opts: &GlobalOpts,
    env: &Env,
    root: &Path,
    config: &Config,
    home: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let target = |flag: &Option<PathBuf>, configured: &str, what: &str| match flag {
        Some(p) => canonical_dir(&env.cwd.join(p), what),
        None => canonical_dir(&root.join(expand_tilde(configured, home)), what),
    };
    let home_target = target(&opts.target, &config.defaults.target, "target")?;
    let root_target = target(&opts.root_target, &config.defaults.root_target, "root target")?;
    for (dir, what) in [(&home_target, "target"), (&root_target, "root target")] {
        if dir.starts_with(root) {
            return Err(Error::Usage(format!(
                "{what} {} is inside the repository {}",
                dir.display(),
                root.display()
            )));
        }
    }
    Ok((home_target, root_target))
}

/// Notices about a private cache and a non-default root target.
fn setup_notices(private_cache: Option<&Path>, root_target: &Path) -> Vec<String> {
    let mut notices = Vec::new();
    if let Some(cache) = private_cache {
        notices.push(format!(
            "the repository belongs to another user; using the private cache {}",
            cache.display()
        ));
    }
    if root_target != Path::new("/") {
        notices.push(format!(
            "system files (.rustowroot/) deploy under {}",
            root_target.display()
        ));
    }
    notices
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
/// directory with `.rustow.toml` or `.rustowignore`; for `init` only, the working directory.
pub fn find_root(opts: &GlobalOpts, env: &Env, notices: &mut Vec<String>, new_repo: bool) -> Result<PathBuf> {
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
    // Only `init` may adopt an unmarked directory; anything else could
    // deploy a random directory (bare `rustow` = apply) into HOME.
    if !new_repo {
        return Err(Error::Usage(format!(
            "no rustow repository found at or above {} (no {CONFIG_FILE} or {IGNORE_FILE}); \
             run `rustow init` in the repository, pass --repo DIR, or set RUSTOW_REPO",
            cwd.display()
        )));
    }
    notices.push(format!(
        "initializing the current directory {} as a new repository",
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
    /// True when this user cannot use the repository's own cache.
    pub private_cache: bool,
}

impl InternalDirs {
    /// Relative values resolve against the repository, never the working directory.
    pub fn resolve(root: &Path, home: &Path, config: &Config, env: &Env) -> Result<InternalDirs> {
        let resolve = |value: &str| normalize(&root.join(expand_tilde(value, home)));
        let cache_dir = locate(resolve(&config.defaults.cache_dir), root);
        let state_dir = locate(
            match (&config.defaults.state_dir, &env.xdg_state_home) {
                (Some(value), _) => resolve(value),
                (None, Some(xdg)) if xdg.is_absolute() => xdg.join("rustow"),
                (None, _) => home.join(".local/state/rustow"),
            },
            root,
        );
        let inside = internal_dirs(root, &[("cache_dir", &cache_dir), ("state_dir", &state_dir)])?;
        let owner = std::fs::metadata(root).map_or(0, |m| std::os::unix::fs::MetadataExt::uid(&m));
        let (cache_dir, private) = cache_for_user(cache_dir, root, owner, nix::unistd::geteuid().as_raw(), &state_dir);
        Ok(InternalDirs {
            cache_dir,
            state_dir,
            inside,
            private_cache: private,
        })
    }
}

/// The in-repository cache belongs to the repository owner. Another user
/// (normally root) gets a private cache in its own state directory, so
/// neither run can block or poison the other. An explicitly configured
/// cache outside the repository is used as is. Returns the cache and
/// whether it was switched.
fn cache_for_user(cache: PathBuf, root: &Path, repo_owner: u32, euid: u32, state_dir: &Path) -> (PathBuf, bool) {
    if cache.starts_with(root) && repo_owner != euid {
        return (state_dir.join("cache"), true);
    }
    (cache, false)
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
        check_components(root, &rel, name)?;
        // The built-in cache is reserved by name and hides no source tree.
        if rel.as_str() != crate::ignore::DEFAULT_CACHE_DIR {
            check_own_contents(dir).map_err(|why| bad(&why))?;
        }
        out.push(rel);
    }
    Ok(out)
}

/// Refuses a symlink or non-directory above an in-repository internal
/// directory, so that no command creates it elsewhere. The directory
/// itself is checked where it is created or used; rustow never follows it.
fn check_components(root: &Path, rel: &RelPath, name: &str) -> Result<()> {
    let Some(parent) = rel.parent() else {
        return Ok(());
    };
    let mut cur = root.to_path_buf();
    for part in parent.components() {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(Error::Conflict(format!(
                    "{name} {}: {} is not a directory (symlinks are not followed); move it away first",
                    root.join(rel.as_path()).display(),
                    cur.display()
                )));
            }
            // Missing parts are created as real directories when needed.
            Err(_) => break,
        }
    }
    Ok(())
}

/// An in-repository internal directory may hold only rustow's entries:
/// anything else would be a source that the built-in exclusion hides.
fn check_own_contents(dir: &Path) -> std::result::Result<(), String> {
    // A symlink is never followed; absent or unreadable directories hide
    // nothing. The writes report the rest.
    let real_dir = std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
    let Some(entries) = real_dir.then(|| std::fs::read_dir(dir).ok()).flatten() else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_own_entry(&name) {
            return Err(format!(
                "holds '{name}', which is not rustow's; an in-repository cache or state directory \
                 may hold only rustow's own files"
            ));
        }
    }
    Ok(())
}

/// Top-level names rustow writes in a cache or state directory.
fn is_own_entry(name: &str) -> bool {
    let repo_id = |stem: &str| stem.len() == 16 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    matches!(name, "rendered" | "decrypted" | "edit" | "backups" | "cache")
        || name.starts_with(crate::fsops::TEMP_PREFIX)
        || [".json", ".lock"]
            .iter()
            .any(|ext| name.strip_suffix(ext).is_some_and(repo_id))
}

/// The form of `path` that compares with the canonical `root`. A path
/// below `root` is kept, so a symlink inside the repository is refused
/// later instead of followed. Otherwise the nearest existing ancestor of
/// the parent is canonicalized and the other components are appended; the
/// last component is never resolved, so a symlinked directory stays visible.
fn locate(path: PathBuf, root: &Path) -> PathBuf {
    if path.starts_with(root) {
        return path;
    }
    let mut tail = Vec::new();
    let mut cur = path.as_path();
    while let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) {
        tail.push(name);
        if let Ok(real) = parent.canonicalize() {
            return tail.iter().rev().fold(real, |p, name| p.join(name));
        }
        cur = parent;
    }
    path
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
    fn no_marker_and_no_flag_is_an_error() {
        let mut f = fx();
        fs::create_dir_all(f.base.join("plain")).unwrap();
        f.env.cwd = f.base.join("plain");
        let e = open(&f, GlobalOpts::default()).err().unwrap().to_string();
        assert!(e.contains("no rustow repository") && e.contains("--repo"), "{e}");
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
    fn internal_dirs_named_through_symlinks_are_inside() {
        let f = fx();
        std::os::unix::fs::symlink(f.base.join("home"), f.base.join("home-link")).unwrap();
        let link = f.base.join("home-link/dot");
        fs::write(
            f.base.join("home/dot/.rustow.toml"),
            format!(
                "version = 1\n[defaults]\ncache_dir = \"{}/var/c\"\nstate_dir = \"~/dot/st\"\n",
                link.display()
            ),
        )
        .unwrap();
        let root = f.base.join("home/dot");
        let config = Config::load(&root).unwrap();
        let dirs = InternalDirs::resolve(&root, &f.base.join("home-link"), &config, &f.env).unwrap();
        assert_eq!(dirs.cache_dir, root.join("var/c"));
        assert_eq!(dirs.state_dir, root.join("st"));
        let inside: Vec<&str> = dirs.inside.iter().map(RelPath::as_str).collect();
        assert_eq!(inside, ["var/c", "st"]);
    }

    #[test]
    fn own_entries_are_cache_state_and_temp_names() {
        for name in [
            "rendered",
            "decrypted",
            "edit",
            "backups",
            "cache",
            "0123456789abcdef.json",
            "0123456789abcdef.lock",
            ".rustow-tmp-Ab12Cd",
        ] {
            assert!(is_own_entry(name), "{name}");
        }
        for name in [
            "nvim",
            "old.json",
            "0123456789ABCDEF.json",
            "0123456789abcdef",
            "x.lock",
            ".rustow-x",
        ] {
            assert!(!is_own_entry(name), "{name}");
        }
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
    fn in_repo_cache_belongs_to_the_repository_owner() {
        let root = Path::new("/home/u/dot");
        let state = Path::new("/root/.local/state/rustow");
        let in_repo = PathBuf::from("/home/u/dot/.rustowcache");
        assert_eq!(
            cache_for_user(in_repo.clone(), root, 1000, 1000, state),
            (in_repo.clone(), false)
        );
        assert_eq!(
            cache_for_user(in_repo, root, 1000, 0, state),
            (state.join("cache"), true)
        );
        let outside = PathBuf::from("/var/cache/rustow");
        assert_eq!(
            cache_for_user(outside.clone(), root, 1000, 0, state),
            (outside, false),
            "explicit cache honored"
        );
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
    fn inactive_template_argument_selects_nothing_without_error() {
        let f = fx();
        fs::write(f.base.join("home/dot/off.tmpl"), "{%- meta.os == \"plan9\" %}\nx\n").unwrap();
        let ctx = open(&f, GlobalOpts::default()).unwrap();
        let d = ctx.discover().unwrap();
        assert!(ctx.select(&d, &["off.tmpl".into()]).unwrap().is_empty());
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
        fs::write(f.base.join("home/dot/.rustowroot/etc/x.conf.tmpl"), "").unwrap();
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
