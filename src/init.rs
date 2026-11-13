//! `rustow init`: prepares a new or existing repository. Never deploys,
//! never runs Git, and never replaces a file it did not expect.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use nix::fcntl::OFlag;

use crate::config::edit;
use crate::config::{CONFIG_FILE, Config, MAX_FILE_SIZE, text_of};
use crate::error::{Error, IoContext, Result};
use crate::fsops;
use crate::ignore::{DEFAULT_CACHE_DIR, IGNORE_FILE};
use crate::paths::RelPath;
use crate::repo::{self, Env, GlobalOpts, InternalDirs};
use crate::report::Change;

#[derive(Debug)]
pub struct InitReport {
    pub root: PathBuf,
    pub notices: Vec<String>,
    pub changes: Vec<Change>,
}

pub fn init(opts: &GlobalOpts, env: &Env, dry_run: bool) -> Result<InitReport> {
    let mut notices = Vec::new();
    let root = repo::find_root(opts, env, &mut notices, true)?;
    if root.parent().is_none() {
        return Err(Error::Usage("refusing to initialize the filesystem root".into()));
    }
    // Every check runs before the first write, so a conflict changes nothing.
    let mut changes = Vec::new();
    let mut writes = Vec::new();
    let cache_dir = plan(&root, env, &mut writes, &mut changes)?;
    if !dry_run {
        for (path, text, mode) in &writes {
            fsops::write_atomic(path, text.as_bytes(), *mode, None, None)?;
        }
        fsops::ensure_private_dir(&cache_dir)?;
    }
    Ok(InitReport { root, notices, changes })
}

/// A file write that init performs once every check has passed: path, text, mode.
type Writes = Vec<(PathBuf, String, u32)>;

/// Checks everything and collects the writes; returns the cache directory to create.
fn plan(root: &Path, env: &Env, writes: &mut Writes, changes: &mut Vec<Change>) -> Result<PathBuf> {
    let config = init_config(root, writes, changes)?;
    let dirs = InternalDirs::resolve(root, &env.home, &config, env)?;
    let mut wanted = vec![RelPath::parse(DEFAULT_CACHE_DIR)?];
    wanted.extend(dirs.inside.iter().filter(|d| d.as_str() != DEFAULT_CACHE_DIR).cloned());
    for name in [".gitignore", IGNORE_FILE] {
        init_ignore_file(root, name, &wanted, writes, changes)?;
    }
    changes.push(cache_dir_change(root, &dirs.cache_dir)?);
    Ok(dirs.cache_dir)
}

fn init_config(root: &Path, writes: &mut Writes, changes: &mut Vec<Change>) -> Result<Config> {
    let path = root.join(CONFIG_FILE);
    let (text, change) = match read_text(&path)? {
        None => {
            let text = edit::minimal();
            writes.push((path, text.clone(), 0o644));
            (text, Change::new(CONFIG_FILE, "create", None))
        }
        Some((text, mode)) => match edit::ensure_init(&text)? {
            Some(new) => {
                writes.push((path, new.clone(), mode));
                (
                    new,
                    Change::new(CONFIG_FILE, "update", Some("added version = 1".into())),
                )
            }
            None => (text, Change::new(CONFIG_FILE, "ok", None)),
        },
    };
    changes.push(change);
    Config::parse(&text)
}

fn init_ignore_file(
    root: &Path,
    name: &str,
    dirs: &[RelPath],
    writes: &mut Writes,
    changes: &mut Vec<Change>,
) -> Result<()> {
    let path = root.join(name);
    let existing = read_text(&path)?;
    let mut text = existing.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
    let mut added = Vec::new();
    for dir in dirs {
        if let Some(updated) = edit::ensure_ignore_entry(&text, dir) {
            text = updated;
            added.push(edit::ignore_entry(dir));
        }
    }
    if added.is_empty() {
        changes.push(Change::new(name, "ok", None));
        return Ok(());
    }
    let action = if existing.is_some() { "update" } else { "create" };
    writes.push((path, text, existing.map_or(0o644, |(_, m)| m)));
    changes.push(Change::new(name, action, Some(format!("added {}", added.join(" ")))));
    Ok(())
}

/// What init will do to the cache directory; a conflict when it is not a real directory.
fn cache_dir_change(root: &Path, dir: &Path) -> Result<Change> {
    let name = dir
        .strip_prefix(root)
        .map_or_else(|_| dir.display().to_string(), |r| format!("{}/", r.display()));
    let action = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => "create",
        Err(e) => return Err(e).ctx(|| format!("inspect {}", dir.display())),
        Ok(m) if m.is_dir() && m.mode() & 0o077 != 0 => "update",
        Ok(m) if m.is_dir() => "ok",
        Ok(_) => {
            return Err(Error::Conflict(format!(
                "{} exists and is not a directory (symlinks are not followed); move it away first",
                dir.display()
            )));
        }
    };
    let detail = (action != "ok").then(|| "mode 0700".to_owned());
    Ok(Change::new(&name, action, detail))
}

/// Text and mode of an existing regular file; `None` when absent. Any
/// other object (symlink, directory) is a conflict that init will not touch.
fn read_text(path: &Path) -> Result<Option<(String, u32)>> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).ctx(|| format!("inspect {}", path.display())),
        Ok(m) => m,
    };
    if !meta.is_file() {
        return Err(Error::Conflict(format!(
            "{} is not a regular file (symlinks are not followed); rustow init will not replace it",
            path.display()
        )));
    }
    let text = text_of(path, fsops::read_limited(path, OFlag::O_NOFOLLOW, MAX_FILE_SIZE)?)?;
    Ok(Some((text, meta.permissions().mode() & 0o7777)))
}
